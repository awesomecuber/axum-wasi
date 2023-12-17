use exports::wasi::http::incoming_handler::Guest;
use wasi::http::types::{IncomingRequest, ResponseOutparam};

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
        todo!()
    }
}
