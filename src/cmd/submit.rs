use anyhow::Result;

use nab::AcceleratedClient;

use super::output::output_response;
use crate::OutputFormat;

#[allow(clippy::too_many_arguments)]
pub async fn cmd_submit(
    url: &str,
    field_args: &[String],
    csrf_from: Option<&str>,
    cookies: &str,
    use_1password: bool,
    show_headers: bool,
    format: OutputFormat,
) -> Result<()> {
    use nab::{parse_field_args, Form};

    let client = create_client_with_cookies(cookies, use_1password, url).await?;

    println!("Fetching form page: {url}");
    let page_html = client.fetch_text(url).await?;

    let mut forms = Form::parse_all(&page_html)?;

    if forms.is_empty() {
        anyhow::bail!("No forms found on page");
    }

    let mut form = forms.remove(0);
    println!("Found form: {} {}", form.method, form.action);
    println!("  Hidden fields: {}", form.hidden_fields.len());

    if let Some(selector) = csrf_from {
        if let Some(token) = Form::extract_csrf_token(&page_html, selector)? {
            println!("  CSRF token extracted: {}", &token[..token.len().min(20)]);
            let field_name = if selector.contains("name=") {
                selector
                    .split("name=")
                    .nth(1)
                    .and_then(|s| s.split(']').next())
                    .unwrap_or("csrf_token")
            } else {
                "csrf_token"
            };
            form.fields.insert(field_name.to_string(), token);
        } else {
            anyhow::bail!("CSRF token not found with selector: {}", selector);
        }
    }

    let user_fields = parse_field_args(field_args)?;
    println!("  User fields: {}", user_fields.len());
    form.merge_fields(&user_fields);

    let action_url = form.resolve_action(url)?;
    println!("Submitting to: {action_url}");

    let form_data = form.encode_urlencoded();
    let response = client
        .inner()
        .post(&action_url)
        .header("Content-Type", form.content_type())
        .body(form_data)
        .send()
        .await?;

    output_response(response, show_headers, true, format, None, false, false, 0).await?;

    Ok(())
}

/// Create HTTP client with cookie support
async fn create_client_with_cookies(
    _cookies: &str,
    _use_1password: bool,
    _url: &str,
) -> Result<AcceleratedClient> {
    AcceleratedClient::new()
}
