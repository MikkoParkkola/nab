use anyhow::Result;

use nab::AcceleratedClient;

use super::output::output_response;

/// All parameters for a `nab submit` invocation.
pub struct SubmitConfig {
    pub url: String,
    pub field_args: Vec<String>,
    pub csrf_from: Option<String>,
    pub show_headers: bool,
}

pub async fn cmd_submit(cfg: &SubmitConfig) -> Result<()> {
    use nab::{Form, parse_field_args};

    let client = AcceleratedClient::new()?;

    println!("Fetching form page: {}", cfg.url);
    let page_html = client.fetch_text(&cfg.url).await?;

    let mut forms = Form::parse_all(&page_html)?;

    if forms.is_empty() {
        anyhow::bail!("No forms found on page");
    }

    let mut form = forms.remove(0);
    println!("Found form: {} {}", form.method, form.action);
    println!("  Hidden fields: {}", form.hidden_fields.len());

    if let Some(selector) = &cfg.csrf_from {
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
            anyhow::bail!("CSRF token not found with selector: {selector}");
        }
    }

    let user_fields = parse_field_args(&cfg.field_args)?;
    println!("  User fields: {}", user_fields.len());
    form.merge_fields(&user_fields);

    let action_url = form.resolve_action(&cfg.url)?;
    println!("Submitting to: {action_url}");

    let form_data = form.encode_urlencoded();
    let response = client
        .inner()
        .post(&action_url)
        .header("Content-Type", form.content_type())
        .body(form_data)
        .send()
        .await?;

    output_response(response, cfg.show_headers).await?;

    Ok(())
}
