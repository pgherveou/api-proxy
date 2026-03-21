use axum::response::Html;
use axum::response::IntoResponse;

pub async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

pub async fn favicon() -> impl IntoResponse {
    (
        [("content-type", "image/svg+xml")],
        include_str!("../static/favicon.svg"),
    )
}
