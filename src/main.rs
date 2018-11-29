use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        mpsc::{channel, Receiver},
        Arc,
    },
};

use actix::{
    ActorContext, ActorFuture, Addr, Arbiter, AsyncContext, Context, ContextFutureSpawner, Handler,
    Message, Recipient, SyncArbiter, SyncContext, WrapFuture,
};
use actix_web::{fs::StaticFiles, server, ws, App, HttpRequest, HttpResponse};
use failure::format_err;
use log::{error, info, warn};
use notify::Watcher;
use serde_derive::{Deserialize, Serialize};
use structopt::StructOpt;

use crate::middleware::ScriptInjector;

mod middleware;

fn run() -> Result<(), failure::Error> {
    std::env::set_var("RUST_LOG", "hot_reload=info");
    env_logger::init();
    let Opt {
        port,
        dir,
        route,
        index_file,
    } = Opt::from_args();

    // little racy
    if !fs::metadata(&dir).map(|md| md.is_dir()).unwrap_or(false) {
        return Err(format_err!("{} is not a directory", dir.display()));
    }

    let dir = Arc::new(dir);

    let ret = actix::System::run(move || {
        let broker = Arbiter::start(|_| WsBroker::new());

        let dir_broker = broker.clone();
        let dir_dir = Arc::clone(&dir);
        let _dir_watcher = SyncArbiter::start(1, move || {
            DirWatcher::new(&*dir_dir, dir_broker.clone()).expect("Can't watch directory")
        });

        let serv_dir = Arc::clone(&dir);
        server::new(move || {
            App::with_state(AppState {
                broker: broker.clone(),
            })
            .middleware(actix_web::middleware::Logger::default())
            .middleware(ScriptInjector::new(port, &route))
            .resource(&route, |r| r.route().f(ws_route))
            .handler(
                "/",
                StaticFiles::new(&*serv_dir)
                    .unwrap()
                    .index_file(index_file.clone()),
            )
        })
        .bind(format!("localhost:{}", port))
        .expect(&format!("Can't start server on port {}", port))
        .start();
        info!("Serving {} on localhost:{}", dir.display(), port);
    });

    std::process::exit(ret);
}

/// A simple hot reloading html server
#[derive(StructOpt)]
struct Opt {
    /// Port used for directory serving
    #[structopt(short = "p", long = "port", default_value = "8080")]
    port: u16,

    /// Route used for websocket connections
    #[structopt(short = "w", long = "ws-route", default_value = "/ws/")]
    route: String,

    /// Index file that will be shown by default when accessing server root
    #[structopt(short = "i", long = "index-file", default_value = "index.html")]
    index_file: String,

    /// Directory to serve
    #[structopt(default_value = ".")]
    dir: PathBuf,
}

struct DirWatcher {
    broker: Addr<WsBroker>,
    _watcher: notify::RecommendedWatcher,
    rx: Receiver<notify::DebouncedEvent>,
}

impl DirWatcher {
    pub fn new<P>(watch_dir: P, broker: Addr<WsBroker>) -> Result<Self, notify::Error>
    where
        P: AsRef<Path>,
    {
        let (tx, rx) = channel();
        let mut watcher = notify::watcher(tx, std::time::Duration::from_secs(10))?;
        watcher.watch(watch_dir, notify::RecursiveMode::Recursive)?;
        Ok(Self {
            broker,
            _watcher: watcher,
            rx,
        })
    }
}

impl actix::Actor for DirWatcher {
    type Context = SyncContext<Self>;

    fn started(&mut self, _ctx: &mut Self::Context) {
        use notify::DebouncedEvent::*;
        loop {
            match self.rx.recv() {
                Ok(evt) => match evt {
                    NoticeWrite(path)
                    | NoticeRemove(path)
                    | Create(path)
                    | Write(path)
                    | Chmod(path)
                    | Remove(path)
                    | Rename(path, _) => {
                        info!("Noticed change on {}, reloading", path.display());
                        self.broker.do_send(FsChange);
                    }
                    Rescan => {}
                    Error(e, _) => {
                        error!("Error while watching file: {}", e);
                    }
                },
                Err(e) => {
                    warn!("Error while watching dir: {}", e);
                }
            }
        }
    }
}

// just reload everything on _every_ change
#[derive(Message)]
struct FsChange;

struct WsBroker {
    ws_subscriptions: slab::Slab<Recipient<Cmd>>,
}

impl WsBroker {
    pub fn new() -> Self {
        Self {
            ws_subscriptions: slab::Slab::with_capacity(16),
        }
    }
}

impl actix::Actor for WsBroker {
    type Context = Context<Self>;
}

#[derive(Message)]
#[rtype(usize)]
struct Connect {
    recept: Recipient<Cmd>,
}

impl Handler<Connect> for WsBroker {
    type Result = usize;

    fn handle(&mut self, msg: Connect, _: &mut Self::Context) -> Self::Result {
        self.ws_subscriptions.insert(msg.recept)
    }
}

#[derive(Message)]
struct Disconnect {
    id: usize,
}

impl Handler<Disconnect> for WsBroker {
    type Result = ();

    fn handle(&mut self, msg: Disconnect, _: &mut Self::Context) -> Self::Result {
        self.ws_subscriptions.remove(msg.id);
    }
}

impl Handler<FsChange> for WsBroker {
    type Result = ();

    fn handle(&mut self, _msg: FsChange, _ctx: &mut Self::Context) -> Self::Result {
        for (_, ws) in &self.ws_subscriptions {
            ws.do_send(Cmd::Reload).unwrap();
        }
    }
}

struct AppState {
    broker: Addr<WsBroker>,
}

fn ws_route(req: &HttpRequest<AppState>) -> Result<HttpResponse, actix_web::Error> {
    ws::start(req, WsNotify { id: 0 })
}

struct WsNotify {
    id: usize,
}

impl actix::Actor for WsNotify {
    type Context = ws::WebsocketContext<Self, AppState>;

    fn started(&mut self, ctx: &mut Self::Context) {
        let addr = ctx.address();
        ctx.state()
            .broker
            .send(Connect {
                recept: addr.recipient(),
            })
            .into_actor(self)
            .then(|res, act, ctx| {
                match res {
                    Ok(res) => act.id = res,
                    Err(e) => {
                        error!("Can't connect to ws broker: {}", e);
                        ctx.stop();
                    }
                };
                actix::fut::ok(())
            })
            .wait(ctx);
    }

    fn stopping(&mut self, ctx: &mut Self::Context) -> actix::Running {
        ctx.state().broker.do_send(Disconnect { id: self.id });
        actix::Running::Stop
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
struct Proto {
    cmd: Cmd,
}

#[derive(Message, Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
enum Cmd {
    Hb,
    Reload,
}

impl Handler<Cmd> for WsNotify {
    type Result = ();
    fn handle(&mut self, msg: Cmd, ctx: &mut Self::Context) {
        ctx.text(serde_json::to_string(&Proto { cmd: msg }).unwrap());
    }
}

impl actix::StreamHandler<ws::Message, ws::ProtocolError> for WsNotify {
    fn handle(&mut self, msg: ws::Message, ctx: &mut Self::Context) {
        match msg {
            ws::Message::Close(_) => {
                ctx.stop();
            }
            _ => {}
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{}", e);
        for cause in e.iter_causes() {
            eprintln!("Caused by: {}", cause);
        }
        std::process::exit(1);
    }
}
