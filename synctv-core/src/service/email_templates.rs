//! Email templates for email binding, password reset, login, and test messages.
//!
//! Uses Handlebars for template rendering with variable substitution

use handlebars::Handlebars;
use serde_json::json;
use std::sync::Arc;

use crate::{InternalExt, Result};

const EMAIL_BIND_TEMPLATE: &str = include_str!("email_templates/email_bind.html.hbs");
const EMAIL_BIND_TEXT_TEMPLATE: &str = include_str!("email_templates/email_bind.txt.hbs");
const PASSWORD_RESET_TEMPLATE: &str = include_str!("email_templates/password_reset.html.hbs");
const PASSWORD_RESET_TEXT_TEMPLATE: &str = include_str!("email_templates/password_reset.txt.hbs");
const EMAIL_LOGIN_TEMPLATE: &str = include_str!("email_templates/email_login.html.hbs");
const EMAIL_LOGIN_TEXT_TEMPLATE: &str = include_str!("email_templates/email_login.txt.hbs");
const EMAIL_REGISTRATION_TEMPLATE: &str =
    include_str!("email_templates/email_registration.html.hbs");
const EMAIL_REGISTRATION_TEXT_TEMPLATE: &str =
    include_str!("email_templates/email_registration.txt.hbs");
const TEST_EMAIL_TEMPLATE: &str = include_str!("email_templates/test_email.html.hbs");
const TEST_EMAIL_TEXT_TEMPLATE: &str = include_str!("email_templates/test_email.txt.hbs");

const TEMPLATE_DEFINITIONS: [(&str, &str, &str); 10] = [
    (
        "email_bind",
        EMAIL_BIND_TEMPLATE,
        "Failed to register email bind template",
    ),
    (
        "email_bind_text",
        EMAIL_BIND_TEXT_TEMPLATE,
        "Failed to register email bind text template",
    ),
    (
        "password_reset",
        PASSWORD_RESET_TEMPLATE,
        "Failed to register password reset template",
    ),
    (
        "password_reset_text",
        PASSWORD_RESET_TEXT_TEMPLATE,
        "Failed to register password reset text template",
    ),
    (
        "email_login",
        EMAIL_LOGIN_TEMPLATE,
        "Failed to register email login template",
    ),
    (
        "email_login_text",
        EMAIL_LOGIN_TEXT_TEMPLATE,
        "Failed to register email login text template",
    ),
    (
        "email_registration",
        EMAIL_REGISTRATION_TEMPLATE,
        "Failed to register email registration template",
    ),
    (
        "email_registration_text",
        EMAIL_REGISTRATION_TEXT_TEMPLATE,
        "Failed to register email registration text template",
    ),
    (
        "test_email",
        TEST_EMAIL_TEMPLATE,
        "Failed to register test email template",
    ),
    (
        "test_email_text",
        TEST_EMAIL_TEXT_TEMPLATE,
        "Failed to register test email text template",
    ),
];

/// Email template manager
pub struct EmailTemplateManager {
    handlebars: Arc<Handlebars<'static>>,
}

impl EmailTemplateManager {
    fn register_templates(handlebars: &mut Handlebars<'static>) -> Result<()> {
        for (name, contents, error_message) in TEMPLATE_DEFINITIONS {
            handlebars
                .register_template_string(name, contents)
                .internal_with_err(error_message)?;
        }

        Ok(())
    }

    /// Create a new email template manager
    pub fn new() -> Result<Self> {
        let mut handlebars = Handlebars::new();
        Self::register_templates(&mut handlebars)?;

        Ok(Self {
            handlebars: Arc::new(handlebars),
        })
    }

    /// Render email bind template
    ///
    /// # Arguments
    /// * `token` - Email bind token
    /// * `expires_in` - Token expiration time (human readable, e.g., "24 hours")
    pub fn render_email_bind_email(
        &self,
        token: &str,
        expires_in: &str,
    ) -> Result<(String, String)> {
        let data = json!({
            "token": token,
            "expires_in": expires_in,
        });

        let html = self
            .handlebars
            .render("email_bind", &data)
            .internal_with_err("Failed to render template")?;

        let plain_text = self
            .handlebars
            .render("email_bind_text", &data)
            .internal_with_err("Failed to render template")?;

        Ok((html, plain_text))
    }

    /// Render password reset template
    ///
    /// # Arguments
    /// * `token` - Reset token
    /// * `expires_in` - Token expiration time (human readable, e.g., "1 hour")
    pub fn render_password_reset_email(
        &self,
        token: &str,
        expires_in: &str,
    ) -> Result<(String, String)> {
        let data = json!({
            "token": token,
            "expires_in": expires_in,
        });

        let html = self
            .handlebars
            .render("password_reset", &data)
            .internal_with_err("Failed to render template")?;

        let plain_text = self
            .handlebars
            .render("password_reset_text", &data)
            .internal_with_err("Failed to render template")?;

        Ok((html, plain_text))
    }

    /// Render passwordless email login template.
    pub fn render_email_login_email(
        &self,
        token: &str,
        expires_in: &str,
    ) -> Result<(String, String)> {
        let data = json!({
            "token": token,
            "expires_in": expires_in,
        });

        let html = self
            .handlebars
            .render("email_login", &data)
            .internal_with_err("Failed to render template")?;

        let plain_text = self
            .handlebars
            .render("email_login_text", &data)
            .internal_with_err("Failed to render template")?;

        Ok((html, plain_text))
    }

    /// Render email registration template.
    pub fn render_email_registration_email(
        &self,
        token: &str,
        expires_in: &str,
    ) -> Result<(String, String)> {
        let data = json!({
            "token": token,
            "expires_in": expires_in,
        });

        let html = self
            .handlebars
            .render("email_registration", &data)
            .internal_with_err("Failed to render template")?;

        let plain_text = self
            .handlebars
            .render("email_registration_text", &data)
            .internal_with_err("Failed to render template")?;

        Ok((html, plain_text))
    }

    /// Render test email template
    ///
    /// # Arguments
    /// * `smtp_host` - SMTP server host
    /// * `smtp_port` - SMTP server port
    /// * `sent_at` - Timestamp of email sending
    pub fn render_test_email(
        &self,
        smtp_host: &str,
        smtp_port: u16,
        sent_at: &str,
    ) -> Result<(String, String)> {
        let data = json!({
            "smtp_host": smtp_host,
            "smtp_port": smtp_port,
            "sent_at": sent_at,
        });

        let html = self
            .handlebars
            .render("test_email", &data)
            .internal_with_err("Failed to render template")?;

        let plain_text = self
            .handlebars
            .render("test_email_text", &data)
            .internal_with_err("Failed to render template")?;

        Ok((html, plain_text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn manager() -> EmailTemplateManager {
        ok(
            EmailTemplateManager::new(),
            "email template manager should build",
        )
    }

    #[test]
    fn test_render_email_bind_email() {
        let manager = manager();
        let result = manager.render_email_bind_email("123456", "24 hours");
        assert!(result.is_ok());

        let (html, plain_text) = ok(result, "email bind template should render");
        assert!(html.contains("123456"));
        assert!(html.contains("24 hours"));
        assert!(plain_text.contains("123456"));
    }

    #[test]
    fn test_render_email_login_email() {
        let manager = manager();
        let result = manager.render_email_login_email("654321", "15 minutes");
        assert!(result.is_ok());

        let (html, plain_text) = ok(result, "email login template should render");
        assert!(html.contains("654321"));
        assert!(html.contains("15 minutes"));
        assert!(plain_text.contains("654321"));
        assert!(plain_text.contains("15 minutes"));
    }

    #[test]
    fn test_render_email_registration_email() {
        let manager = manager();
        let result = manager.render_email_registration_email("654321", "15 minutes");
        assert!(result.is_ok());

        let (html, plain_text) = ok(result, "email registration template should render");
        assert!(html.contains("654321"));
        assert!(html.contains("15 minutes"));
        assert!(plain_text.contains("654321"));
        assert!(plain_text.contains("15 minutes"));
    }

    #[test]
    fn test_render_password_reset_email() {
        let manager = manager();
        let result = manager.render_password_reset_email("ABC123", "1 hour");
        assert!(result.is_ok());

        let (html, plain_text) = ok(result, "password reset template should render");
        assert!(html.contains("ABC123"));
        assert!(html.contains("1 hour"));
        assert!(plain_text.contains("ABC123"));
    }

    #[test]
    fn test_render_test_email() {
        let manager = manager();
        let result = manager.render_test_email("smtp.example.com", 587, "2024-01-01 12:00:00");
        assert!(result.is_ok());

        let (html, plain_text) = ok(result, "test email template should render");
        assert!(html.contains("smtp.example.com"));
        assert!(html.contains("587"));
        assert!(plain_text.contains("smtp.example.com:587"));
    }
}
