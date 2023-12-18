use std::{
    cell::RefCell,
    fmt::Display,
    future, mem,
    rc::Rc,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker},
};

use axum::{
    body::Body,
    http::{Request, Uri},
    response::{Html, Response},
    routing::get,
    Router,
};
use exports::wasi::http::incoming_handler::Guest;
use futures::{sink, stream, Future, Sink, Stream, StreamExt};
use tower::Service;
use wasi::{
    http::types::{
        Fields, IncomingBody, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
    },
    io,
};

use crate::wasi::io::streams::{OutputStream, StreamError};

wit_bindgen::generate!({
    world: "target-world",
    exports: {
        "wasi:http/incoming-handler": Handler,
    },
});

struct Handler;

impl Guest for Handler {
    #[doc = " This function is invoked with an incoming HTTP Request, and a resource"]
    #[doc = " `response-outparam` which provides the capability to reply with an HTTP"]
    #[doc = " Response. The response is sent by calling the `response-outparam.set`"]
    #[doc = " method, which allows execution to continue after the response has been"]
    #[doc = " sent. This enables both streaming to the response body, and performing other"]
    #[doc = " work."]
    #[doc = " "]
    #[doc = " The implementor of this function must write a response to the"]
    #[doc = " `response-outparam` before returning, or else the caller will respond"]
    #[doc = " with an error on its behalf."]
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let uri = {
            let mut uri_builder = Uri::builder();
            if let Some(scheme) = request.scheme() {
                let scheme_str = match scheme {
                    wasi::http::types::Scheme::Http => "http",
                    wasi::http::types::Scheme::Https => "https",
                    wasi::http::types::Scheme::Other(ref other) => other,
                };
                uri_builder = uri_builder.scheme(scheme_str);
            }
            if let Some(authority) = request.authority() {
                uri_builder = uri_builder.authority(authority);
            }
            if let Some(path_and_query) = request.path_with_query() {
                uri_builder = uri_builder.path_and_query(path_and_query);
            }
            uri_builder.build().unwrap()
        };

        let method = request.method();
        let method = match method {
            wasi::http::types::Method::Get => "GET",
            wasi::http::types::Method::Head => "HEAD",
            wasi::http::types::Method::Post => "POST",
            wasi::http::types::Method::Put => "PUT",
            wasi::http::types::Method::Delete => "DELETE",
            wasi::http::types::Method::Connect => "CONNECT",
            wasi::http::types::Method::Options => "OPTIONS",
            wasi::http::types::Method::Trace => "TRACE",
            wasi::http::types::Method::Patch => "PATCH",
            wasi::http::types::Method::Other(ref other) => other,
        };

        let incoming_body = request.consume().unwrap();
        let body_stream = incoming_body_to_stream(incoming_body);

        let body = Body::from_stream(body_stream);

        let mut request_builder = Request::builder().method(method).uri(uri);
        for (n, v) in request.headers().entries().into_iter() {
            request_builder = request_builder.header(n, v);
        }
        let request = request_builder.body(body).unwrap();

        let (_parts, body) = block_on(app(request)).into_parts();

        let _body = body.into_data_stream().collect::<Vec<_>>();

        let response_headers = Fields::from_list(&[]).unwrap();
        let outgoing_response = OutgoingResponse::new(response_headers);
        let _outgoing_body = outgoing_response.body().unwrap();

        ResponseOutparam::set(response_out, Ok(OutgoingResponse::new(Fields::new())))
    }
}

async fn app(request: Request<Body>) -> Response {
    let mut router = Router::new().route("/api/", get(index));
    router.call(request).await.unwrap()
}

async fn index() -> Html<&'static str> {
    Html("<h1>Hello, world!</h1>")
}

static WAKERS: Mutex<Vec<(io::poll::Pollable, Waker)>> = Mutex::new(Vec::new());

pub fn block_on<T>(future: impl Future<Output = T>) -> T {
    futures::pin_mut!(future);
    struct DummyWaker;

    impl Wake for DummyWaker {
        fn wake(self: Arc<Self>) {}
    }

    let waker = Arc::new(DummyWaker).into();

    loop {
        match future.as_mut().poll(&mut Context::from_waker(&waker)) {
            Poll::Pending => {
                let mut new_wakers = Vec::new();

                let wakers = mem::take::<Vec<_>>(&mut WAKERS.lock().unwrap());

                assert!(!wakers.is_empty());

                let pollables = wakers
                    .iter()
                    .map(|(pollable, _)| pollable)
                    .collect::<Vec<_>>();

                let mut ready = vec![false; wakers.len()];

                for index in io::poll::poll(&pollables) {
                    ready[usize::try_from(index).unwrap()] = true;
                }

                for (ready, (pollable, waker)) in ready.into_iter().zip(wakers) {
                    if ready {
                        waker.wake()
                    } else {
                        new_wakers.push((pollable, waker));
                    }
                }

                *WAKERS.lock().unwrap() = new_wakers;
            }
            Poll::Ready(result) => break result,
        }
    }
}

const READ_SIZE: u64 = 16 * 1024;

#[doc(hidden)]
pub fn incoming_body_to_stream(
    body: IncomingBody,
) -> impl Stream<Item = Result<Vec<u8>, io::streams::Error>> {
    // TODO: No trailer support!
    let to_capture = (body.stream().unwrap(), body);

    stream::poll_fn(move |context| {
        let (stream, _body) = &to_capture;
        match stream.read(READ_SIZE) {
            Ok(buf) if !buf.is_empty() => Poll::Ready(Some(Ok(buf))),
            Ok(_) => {
                WAKERS
                    .lock()
                    .unwrap()
                    .push((stream.subscribe(), context.waker().clone()));
                Poll::Pending
            }
            Err(StreamError::Closed) => Poll::Ready(None),
            Err(StreamError::LastOperationFailed(e)) => Poll::Ready(Some(Err(e))),
        }
    })
}

impl Display for wasi::io::error::Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_debug_string())
    }
}

impl std::error::Error for wasi::io::error::Error {}

fn _outgoing_body_to_sink(body: OutgoingBody) -> impl Sink<Vec<u8>, Error = io::streams::Error> {
    struct Outgoing(Option<(OutputStream, OutgoingBody)>);

    impl Drop for Outgoing {
        fn drop(&mut self) {
            if let Some((stream, body)) = self.0.take() {
                drop(stream);
                let _ = OutgoingBody::finish(body, None);
            }
        }
    }

    let stream = body.write().expect("response body should be writable");
    let pair = Rc::new(RefCell::new(Outgoing(Some((stream, body)))));

    sink::unfold((), {
        move |(), chunk: Vec<u8>| {
            future::poll_fn({
                let mut offset = 0;
                let mut flushing = false;
                let pair = pair.clone();

                move |context| {
                    let pair = pair.borrow();
                    let (stream, _) = &pair.0.as_ref().unwrap();
                    loop {
                        match stream.check_write() {
                            Ok(0) => {
                                WAKERS
                                    .lock()
                                    .unwrap()
                                    .push((stream.subscribe(), context.waker().clone()));

                                break Poll::Pending;
                            }
                            Ok(count) => {
                                if offset == chunk.len() {
                                    if flushing {
                                        break Poll::Ready(Ok(()));
                                    } else {
                                        match stream.flush() {
                                            Ok(()) => flushing = true,
                                            Err(StreamError::Closed) => break Poll::Ready(Ok(())),
                                            Err(StreamError::LastOperationFailed(e)) => {
                                                break Poll::Ready(Err(e))
                                            }
                                        }
                                    }
                                } else {
                                    let count =
                                        usize::try_from(count).unwrap().min(chunk.len() - offset);

                                    match stream.write(&chunk[offset..][..count]) {
                                        Ok(()) => {
                                            offset += count;
                                        }
                                        Err(StreamError::Closed) => break Poll::Ready(Ok(())),
                                        Err(StreamError::LastOperationFailed(e)) => {
                                            break Poll::Ready(Err(e))
                                        }
                                    }
                                }
                            }
                            Err(StreamError::Closed) => break Poll::Ready(Ok(())),
                            Err(StreamError::LastOperationFailed(e)) => break Poll::Ready(Err(e)),
                        }
                    }
                }
            })
        }
    })
}
