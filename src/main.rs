use std::{convert::TryFrom, net::SocketAddr, sync::Arc};

use axum::{http::Uri, response::Html, routing::get, Extension, Router, Server};
use gmi::{
    gemtext::{self, GemtextNode},
    protocol::StatusCode,
    request,
    url::Url,
};
use handlebars::Handlebars;
use log::LevelFilter;

#[derive(Clone)]
struct AppState {
    template_registry: Handlebars<'static>,
    max_redirects: usize,
}

#[derive(serde::Serialize)]
struct Page {
    error: Option<String>,
    gemini_url: Option<String>,
    content: Option<String>,
}

/// A simple gemini proxy for the web.
#[derive(argh::FromArgs)]
struct Args {
    /// which IP and port to broadcast the server on formatted as ip:port.
    #[argh(
        option,
        short = 'i',
        default = r#"SocketAddr::from(([127, 0, 0, 1], 1414))"#
    )]
    socket: SocketAddr,
    /// maximum number of gemview redirects
    #[argh(option, short = 'r', default = "5")]
    max_redirects: usize,
}

#[tokio::main]
async fn main() {
    pretty_env_logger::formatted_builder()
        .filter_level(LevelFilter::Info)
        .init();

    let args: Args = argh::from_env();

    let mut template_registry = Handlebars::new();
    template_registry
        .register_template_string("page", include_str!("page.html"))
        .unwrap();

    let shared_state = Arc::new(AppState {
        template_registry,
        max_redirects: args.max_redirects,
    });

    let proxy = Router::new().fallback(gemini);

    let app = Router::new()
        .nest("/gemini", proxy)
        .route("/", get(index))
        .layer(Extension(shared_state));

    log::info!("Server started at http://{}", args.socket);
    Server::bind(&SocketAddr::from(args.socket))
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn index(Extension(state): Extension<Arc<AppState>>) -> Html<String> {
    let rendered = match state.template_registry.render(
        "page",
        &Page {
            error: None,
            gemini_url: None,
            content: None,
        },
    ) {
        Ok(rendered) => rendered,
        Err(err) => format!("<p>Error rendering template: {err}</p>"),
    };

    Html::from(rendered)
}

async fn gemini(uri: Uri, Extension(state): Extension<Arc<AppState>>) -> Html<String> {
    let mut page = Page {
        error: None,
        gemini_url: Some(format!("gemini:/{}", uri.path())),
        content: None,
    };

    log::info!("Accessing {}", page.gemini_url.as_ref().unwrap());

    let url: Option<Url> = match Url::try_from(page.gemini_url.as_ref().unwrap().as_str()) {
        Ok(url) => Some(url),
        Err(err) => {
            page.error = Some(err.to_string());

            None
        }
    };

    let mut gemtext: Option<String> = None;

    if let Some(mut url) = url {
        for _ in 0..state.max_redirects {
            let response = match request::make_request(&url) {
                Ok(response) => response,
                Err(err) => {
                    page.error = Some(err.to_string());

                    break;
                }
            };

            match response.status {
                StatusCode::Success(_) => {
                    gemtext = Some(String::from_utf8_lossy(&response.data).to_string());
                    break;
                }
                StatusCode::Redirect(_) => {
                    url = match Url::try_from(response.meta.as_str()) {
                        Ok(url) => url,
                        Err(err) => {
                            page.error = Some(err.to_string());

                            break;
                        }
                    };
                }
                s => {
                    page.error = Some(format!("Unknown status code: {:?}", s));

                    break;
                }
            }
        }
    }

    if let Some(gemtext) = gemtext {
        page.content = Some(gemtext_to_html(gemtext, &uri));
    }

    if let Some(error) = &page.error {
        log::error!("Error accessing {uri}: {error}");
    }

    let rendered = match state.template_registry.render("page", &page) {
        Ok(rendered) => rendered,
        Err(err) => format!("<p>Error rendering template: {err}</p>"),
    };

    Html::from(rendered)
}

fn gemtext_to_html(text: String, base: &Uri) -> String {
    let gemtext = gemtext::parse_gemtext(&text);

    let mut output = String::new();

    for node in gemtext {
        output.push_str(&match node {
            GemtextNode::Text(t) => format!("<p>{t}</p>"),
            GemtextNode::Link(link, desc) => {
                if link.contains("http://") || link.contains("https://") {
                    // http
                    format!("<p><a href=\"{}\">{}</a></p>", link, desc.unwrap_or_default())
                } else if link.contains("gemini://") {
                    // absolute
                    format!(
                        "<p><a href=\"/gemini{}\">{}</a></p>",
                        link.replace("gemini:/", ""),
                        desc.unwrap_or_default()
                    )
                } else {
                    format!("<p><a href=\"/gemini{}/{}\">{}</a> ({link})</p>", base.path(), link, desc.unwrap_or_default())
                }
            }
            GemtextNode::Heading(t) => format!("<h1>{t}</h1>"),
            GemtextNode::SubHeading(t) => format!("<h2>{t}</h2>"),
            GemtextNode::SubSubHeading(t) => format!("<h3>{t}</h3>"),
            GemtextNode::ListItem(t) => format!("<p>&#x2022 {t}</p>"),
            GemtextNode::Blockquote(t) => format!("<blockquote>{t}</blockquote>"),
            GemtextNode::Preformatted(t, f) => match f {
                Some(f) => format!("<pre><code class=\"language-{f}\">{t}</code></pre>"),
                None => format!("<pre><code>{t}</code></pre>"),
            },
            GemtextNode::EmptyLine => String::new(),
        });
    }

    output
}
