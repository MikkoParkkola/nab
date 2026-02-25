//! Demonstration of SPA login form detection with `QuickJS`
//!
//! This example shows how nab can detect and extract login forms
//! from Single Page Applications (SPAs) that render forms via JavaScript.
//!
//! Run with: `cargo run --example spa_login_demo`

use nab::js_engine::JsEngine;

fn main() -> anyhow::Result<()> {
    // Simulate a SPA login page that renders form via inline JavaScript
    let spa_html = r#"
        <!DOCTYPE html>
        <html>
            <head>
                <title>SPA Login Example</title>
            </head>
            <body>
                <div id="root"></div>
                <script>
                    // Simulate React/Vue component rendering
                    var loginForm = document.createElement('form');
                    loginForm.setAttribute('action', '/api/login');
                    loginForm.setAttribute('method', 'POST');

                    // Username field
                    var usernameDiv = document.createElement('div');
                    var usernameLabel = document.createElement('label');
                    usernameLabel.innerText = 'Email: ';
                    var usernameInput = document.createElement('input');
                    usernameInput.setAttribute('name', 'email');
                    usernameInput.setAttribute('type', 'email');
                    usernameInput.setAttribute('required', 'true');
                    usernameDiv.appendChild(usernameLabel);
                    usernameDiv.appendChild(usernameInput);

                    // Password field
                    var passwordDiv = document.createElement('div');
                    var passwordLabel = document.createElement('label');
                    passwordLabel.innerText = 'Password: ';
                    var passwordInput = document.createElement('input');
                    passwordInput.setAttribute('name', 'password');
                    passwordInput.setAttribute('type', 'password');
                    passwordInput.setAttribute('required', 'true');
                    passwordDiv.appendChild(passwordLabel);
                    passwordDiv.appendChild(passwordInput);

                    // Submit button
                    var submitButton = document.createElement('button');
                    submitButton.setAttribute('type', 'submit');
                    submitButton.innerText = 'Login';

                    // Assemble form
                    loginForm.appendChild(usernameDiv);
                    loginForm.appendChild(passwordDiv);
                    loginForm.appendChild(submitButton);

                    // Inject into DOM
                    var root = document.getElementById('root');
                    if (root) {
                        root.appendChild(loginForm);
                    }
                </script>
            </body>
        </html>
    "#;

    println!("🔍 Original HTML (SPA - no static form):\n");
    println!("{spa_html}\n");

    // Create JavaScript engine and execute inline scripts
    let js_engine = JsEngine::new()?;
    let rendered_html = js_engine.execute_and_extract_forms(spa_html)?;

    println!("✨ Rendered HTML (after JavaScript execution):\n");
    println!("{rendered_html}\n");

    // Verify form fields were extracted
    if rendered_html.contains("email") && rendered_html.contains("password") {
        println!("✓ Success! Login form fields detected:");
        println!("  - Email input field");
        println!("  - Password input field");
        println!("\n💡 The login flow can now proceed with form submission!");
    } else {
        println!("❌ Failed to extract login form fields");
    }

    Ok(())
}
