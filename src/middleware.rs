use std::fs;

use actix_web::{
    middleware::{Middleware, Response},
    HttpRequest, HttpResponse,
};
use lazy_static::lazy_static;
use regex::Regex;

#[derive(Clone, Debug)]
pub struct ScriptInjector {
    replacement: String,
}

impl ScriptInjector {
    pub fn new(port: u16, route: &str) -> Self {
        let mut replacement = format!(
            "<head><script>const ws = new WebSocket(\"ws://localhost:{}{}\");",
            port, route
        );
        replacement.push_str(
            r#"
        ws.onmessage = evt => {
            try {
                const msg = JSON.parse(evt.data);
                switch(msg.cmd) {
                    case "reload":
                        ws.onclose = undefined;
                        location.reload();
                        break;
                }

            } catch(e) {
                console.error(`Failed parsing server data: ${e}`);
            }
        };
        ws.onclose = evt => {
            document.write("Server closed connection, probably dead");
            document.close();
        };
        </script>
        "#,
        );
        Self { replacement }
    }
}

impl<S> Middleware<S> for ScriptInjector {
    fn response(&self, req: &HttpRequest<S>, resp: HttpResponse) -> actix_web::Result<Response> {
        lazy_static! {
            static ref TERRIBLE: Regex = Regex::new("<head>").unwrap();
        };
        if req.path().ends_with(".html") || req.path().ends_with(".htm") {
            match fs::read_to_string(req.path().trim_start_matches('/')) {
                Ok(cont) => {
                    let body = TERRIBLE
                        .replace(&cont, regex::NoExpand(&self.replacement))
                        .to_string();
                    let new_resp = HttpResponse::Ok().body(body);
                    Ok(Response::Done(new_resp))
                }
                Err(_) => Ok(Response::Done(resp)),
            }
        } else {
            Ok(Response::Done(resp))
        }
    }
}
