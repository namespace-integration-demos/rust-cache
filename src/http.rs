use std::{
    collections::HashMap,
    hash::Hash,
    fmt::{Debug, Display, Formatter},
    io::Cursor,
    path::Path,
};

use maplit::hashmap;
use tokio::{
    fs::File,
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, AsyncBufRead, AsyncBufReadExt},
};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Request {
    pub method: Method,
    pub path: String,
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Method {
    Get,
}

impl TryFrom<&str> for Method {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "GET" => Ok(Method::Get),
            m => Err(anyhow::anyhow!("unsupported method: {m}")),
        }
    }
}

pub async fn parse_request(mut stream: impl AsyncBufRead + Unpin) -> anyhow::Result<Request> {
    let mut line_buffer = String::new();
    stream.read_line(&mut line_buffer).await?;

    let mut parts = line_buffer.split_whitespace();

    let method: Method = parts
        .next()
        .ok_or(anyhow::anyhow!("missing method"))
        .and_then(TryInto::try_into)?;

    let path: String = parts
        .next()
        .ok_or(anyhow::anyhow!("missing path"))
        .map(Into::into)?;

    let mut headers = HashMap::new();

    loop {
        line_buffer.clear();
        stream.read_line(&mut line_buffer).await?;

        if line_buffer.is_empty() || line_buffer == "\n" || line_buffer == "\r\n" {
            break;
        }

        let mut comps = line_buffer.split(":");
        let key = comps.next().ok_or(anyhow::anyhow!("missing header name"))?;
        let value = comps
            .next()
            .ok_or(anyhow::anyhow!("missing header value"))?
            .trim();

        headers.insert(key.to_string(), value.to_string());
    }

    Ok(Request {
        method,
        path,
        headers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use indoc::indoc;
    use maplit::hashmap;

    #[tokio::test]
    async fn no_headers() {
        let mut stream = Cursor::new("GET /foo HTTP/1.1\r\n");
        let req = parse_request(&mut stream).await.unwrap();
    }

    #[tokio::test]
    async fn test_parse_request() {
        let mut stream = Cursor::new(indoc!(
            "
            GET /foo HTTP/1.1\r\n\
            Host: localhost\r\n\
            \r\n"
        ));
        let req = parse_request(&mut stream).await.unwrap();

        assert_eq!(
            req,
            Request {
                method: Method::Get,
                path: "/foo".to_string(),
                headers: hashmap! { "Host".to_string() => "localhost".to_string() }
            }
        )
    }
}


pub struct Response {
    pub status: Status,
    pub headers: HashMap<String, String>,
    pub data: Box<dyn AsyncRead + Unpin + Send>,
}

impl Response {
    pub fn status_and_headers(&self) -> String {
        let headers = self
            .headers
            .iter()
            .map(|(k, v)| format!("{}: {}", k, v))
            .collect::<Vec<_>>()
            .join("\r\n");

        format!("HTTP/1.1 {}\r\n{headers}\r\n\r\n", self.status)
    }

    pub async fn write<O: AsyncWrite + Unpin>(mut self, stream: &mut O) -> anyhow::Result<()> {
        let bytes = self.status_and_headers().into_bytes();

        stream.write_all(&bytes).await?;

        tokio::io::copy(&mut self.data, stream).await?;

        Ok(())
    }

    pub fn from_html(status: Status, data: impl ToString) -> Self {
        let bytes = data.to_string().into_bytes();

        let headers = hashmap! {
            "Content-Type".to_string() => "text/html".to_string(),
            "Content-Length".to_string() => bytes.len().to_string(),
        };

        Self {
            status,
            headers,
            data: Box::new(Cursor::new(bytes)),
        }
    }

    pub async fn from_file(path: &Path, file: File) -> anyhow::Result<Response> {
        let headers = hashmap! {
            "Content-Length".to_string() => file.metadata().await?.len().to_string(),
            "Content-Type".to_string() => mime_type(path).to_string(),
        };

        Ok(Response {
            headers,
            status: Status::Ok,
            data: Box::new(file),
        })
    }
}

fn mime_type(path: &Path) -> &str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html",
        Some("css") => "text/css",
        Some("js") => "text/javascript",
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("gif") => "image/gif",
        _ => "application/octet-stream",
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Status {
    NotFound,
    Ok,
}

impl Display for Status {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::NotFound => write!(f, "404 Not Found"),
            Status::Ok => write!(f, "200 OK"),
        }
    }
}