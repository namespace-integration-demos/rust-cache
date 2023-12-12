use std::{env::current_dir, io, net::SocketAddr, path::PathBuf};

use clap::Parser;
use tokio::{
    io::{AsyncWrite, BufStream},
    net::{TcpListener, TcpStream},
    signal,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::{
    http::{Request, Response, Status},
};

#[derive(Debug, Clone)]
pub struct StaticFileHandler {
    root: PathBuf,
}

mod http;

impl StaticFileHandler {
    pub fn in_current_dir() -> io::Result<StaticFileHandler> {
        current_dir().map(StaticFileHandler::with_root)
    }

    pub fn with_root(root: PathBuf) -> StaticFileHandler {
        StaticFileHandler { root }
    }

    pub async fn handle(&self, request: Request) -> anyhow::Result<Response> {
        let path = self.root.join(request.path.strip_prefix('/').unwrap());

        if !path.is_file() {
            return Ok(Response::from_html(
                Status::NotFound,
                include_str!("../static/404.html"),
            ));
        }

        let file = tokio::fs::File::open(&path).await?;
        Response::from_file(&path, file).await
    }
}

#[derive(Parser, Debug)]
pub struct Args {
    #[arg(short, long, default_value_t = 8080)]
    pub port: u16,
    #[arg(short, long)]
    pub root: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize the default tracing subscriber.
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let port = args.port;
    let handler = args
        .root
        .map(StaticFileHandler::with_root)
        .unwrap_or_else(|| {
            StaticFileHandler::in_current_dir().expect("failed to get current dir")
        });

    let listener = TcpListener::bind(format!("0.0.0.0:{port}")).await.unwrap();

    info!("listening on: {}", listener.local_addr()?);

    let cancel_token = CancellationToken::new();

    tokio::spawn({
        let cancel_token = cancel_token.clone();
        async move {
            if let Ok(()) = signal::ctrl_c().await {
                info!("received Ctrl-C, shutting down");
                cancel_token.cancel();
            }
        }
    });

    let mut tasks = Vec::new();

    loop {
        let cancel_token = cancel_token.clone();

        tokio::select! {
            Ok((stream, addr)) = listener.accept() => {
                let handler = handler.clone();
                let client_task = tokio::spawn(async move {
                    if let Err(e) = handle_client(cancel_token, stream, addr, &handler).await {
                        error!(?e, "failed to handle client");
                    }
                });
                tasks.push(client_task);
            },
            _ = cancel_token.cancelled() => {
                info!("stop listening");
                break;
            }
        }
    }

    futures::future::join_all(tasks).await;

    Ok(())
}

async fn handle_client(
    cancel_token: CancellationToken,
    stream: TcpStream,
    addr: SocketAddr,
    handler: &StaticFileHandler,
) -> anyhow::Result<()> {
    let mut stream = BufStream::new(stream);

    info!(?addr, "new connection");

    loop {
        tokio::select! {
            req = http::parse_request(&mut stream) => {
                match req {
                    Ok(req) => {
                        info!(?req, "incoming request");
                        let close_conn = handle_req(req, &handler, &mut stream).await?;
                        if close_conn {
                            break;
                        }
                    }
                    Err(e) => {
                        error!(?e, "failed to parse request");
                        break;
                    }
                }
            }
            _ = cancel_token.cancelled() => {
                info!(?addr, "closing connection");
                break;
            }
        }
    }

    Ok(())
}

async fn handle_req<S: AsyncWrite + Unpin>(
    req: http::Request,
    handler: &StaticFileHandler,
    stream: &mut S,
) -> anyhow::Result<bool> {
    let close_connection = req.headers.get("Connection") == Some(&"close".to_string());

    match handler.handle(req).await {
        Ok(resp) => {
            resp.write(stream).await.unwrap();
        }
        Err(e) => {
            error!(?e, "failed to handle request");
            return Ok(false);
        }
    };

    Ok(close_connection)
}