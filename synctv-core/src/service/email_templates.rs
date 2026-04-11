//! Email templates for verification, password reset, and notifications
//!
//! Uses Handlebars for template rendering with variable substitution

use handlebars::Handlebars;
use serde_json::json;
use std::sync::Arc;

use crate::{Error, InternalExt, Result};

const EMAIL_VERIFICATION_TEMPLATE: &str =
    include_str!("email_templates/email_verification.html.hbs");
const EMAIL_VERIFICATION_TEXT_TEMPLATE: &str =
    include_str!("email_templates/email_verification.txt.hbs");
const PASSWORD_RESET_TEMPLATE: &str = include_str!("email_templates/password_reset.html.hbs");
const PASSWORD_RESET_TEXT_TEMPLATE: &str = include_str!("email_templates/password_reset.txt.hbs");
const EMAIL_LOGIN_TEMPLATE: &str = include_str!("email_templates/email_login.html.hbs");
const EMAIL_LOGIN_TEXT_TEMPLATE: &str = include_str!("email_templates/email_login.txt.hbs");
const TEST_EMAIL_TEMPLATE: &str = include_str!("email_templates/test_email.html.hbs");
const TEST_EMAIL_TEXT_TEMPLATE: &str = include_str!("email_templates/test_email.txt.hbs");
const NOTIFICATION_TEMPLATE: &str = include_str!("email_templates/notification.html.hbs");
const NOTIFICATION_TEXT_TEMPLATE: &str = include_str!("email_templates/notification.txt.hbs");

const TEMPLATE_DEFINITIONS: [(&str, &str, &str); 10] = [
    (
        "email_verification",
        EMAIL_VERIFICATION_TEMPLATE,
        "Failed to register email verification template",
    ),
    (
        "email_verification_text",
        EMAIL_VERIFICATION_TEXT_TEMPLATE,
        "Failed to register email verification text template",
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
        "test_email",
        TEST_EMAIL_TEMPLATE,
        "Failed to register test email template",
    ),
    (
        "test_email_text",
        TEST_EMAIL_TEXT_TEMPLATE,
        "Failed to register test email text template",
    ),
    (
        "notification",
        NOTIFICATION_TEMPLATE,
        "Failed to register notification template",
    ),
    (
        "notification_text",
        NOTIFICATION_TEXT_TEMPLATE,
        "Failed to register notification text template",
    ),
];

/// Email template type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailTemplateType {
    /// Email verification template
    EmailVerification,
    /// Password reset template
    PasswordReset,
    /// Passwordless email login template
    EmailLogin,
    /// Test email template
    TestEmail,
    /// General notification template
    Notification,
}

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

    /// Render email verification template
    ///
    /// # Arguments
    /// * `token` - Verification token
    /// * `expires_in` - Token expiration time (human readable, e.g., "24 hours")
    pub fn render_verification_email(
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
            .render("email_verification", &data)
            .internal_with_err("Failed to render template")?;

        let plain_text = self
            .handlebars
            .render("email_verification_text", &data)
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

    /// Render notification email template
    ///
    /// # Arguments
    /// * `title` - Notification title
    /// * `message` - Notification message
    /// * `action_text` - Optional action button text
    /// * `action_url` - Optional action button URL
    pub fn render_notification_email(
        &self,
        title: &str,
        message: &str,
        action_text: Option<&str>,
        action_url: Option<&str>,
    ) -> Result<(String, String)> {
        // Validate action_url scheme to prevent XSS via javascript:/data:/vbscript: URIs
        if let Some(url) = action_url {
            let lower = url.trim().to_lowercase();
            if !(lower.starts_with("https://") || lower.starts_with("http://")) {
                return Err(Error::InvalidInput(
                    "action_url must use http:// or https:// scheme".to_string(),
                ));
            }
        }

        let data = json!({
            "title": title,
            "message": message,
            "action_text": action_text,
            "action_url": action_url,
            "has_action": action_text.is_some() && action_url.is_some(),
        });

        let html = self
            .handlebars
            .render("notification", &data)
            .internal_with_err("Failed to render template")?;

        let plain_text = self
            .handlebars
            .render("notification_text", &data)
            .internal_with_err("Failed to render template")?;

        Ok((html, plain_text))
    }
}

impl Default for EmailTemplateManager {
    fn default() -> Self {
        Self::new().expect("Failed to create default EmailTemplateManager")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_template_manager() {
        let manager = EmailTemplateManager::new();
        assert!(manager.is_ok());
    }

    #[test]
    fn test_render_verification_email() {
        let manager = EmailTemplateManager::new().unwrap();
        let result = manager.render_verification_email("123456", "24 hours");
        assert!(result.is_ok());

        let (html, plain_text) = result.unwrap();
        assert!(html.contains("123456"));
        assert!(html.contains("24 hours"));
        assert!(plain_text.contains("123456"));
    }

    #[test]
    fn test_render_email_login_email() {
        let manager = EmailTemplateManager::new().unwrap();
        let result = manager.render_email_login_email("654321", "15 minutes");
        assert!(result.is_ok());

        let (html, plain_text) = result.unwrap();
        assert!(html.contains("654321"));
        assert!(html.contains("15 minutes"));
        assert!(plain_text.contains("654321"));
        assert!(plain_text.contains("15 minutes"));
    }

    #[test]
    fn test_render_password_reset_email() {
        let manager = EmailTemplateManager::new().unwrap();
        let result = manager.render_password_reset_email("ABC123", "1 hour");
        assert!(result.is_ok());

        let (html, plain_text) = result.unwrap();
        assert!(html.contains("ABC123"));
        assert!(html.contains("1 hour"));
        assert!(plain_text.contains("ABC123"));
    }

    #[test]
    fn test_render_test_email() {
        let manager = EmailTemplateManager::new().unwrap();
        let result = manager.render_test_email("smtp.example.com", 587, "2024-01-01 12:00:00");
        assert!(result.is_ok());

        let (html, plain_text) = result.unwrap();
        assert!(html.contains("smtp.example.com"));
        assert!(html.contains("587"));
        assert!(plain_text.contains("smtp.example.com:587"));
    }

    #[test]
    fn test_render_notification_email() {
        let manager = EmailTemplateManager::new().unwrap();

        // Without action button
        let result = manager.render_notification_email(
            "System Update",
            "The system has been updated successfully.",
            None,
            None,
        );
        assert!(result.is_ok());

        // With action button
        let result = manager.render_notification_email(
            "New Message",
            "You have a new message in your inbox.",
            Some("View Message"),
            Some("https://example.com/messages"),
        );
        assert!(result.is_ok());

        let (html, _) = result.unwrap();
        assert!(html.contains("View Message"));
        assert!(html.contains("https://example.com/messages"));
    }

    #[test]
    fn test_notification_rejects_javascript_url() {
        let manager = EmailTemplateManager::new().unwrap();

        // javascript: scheme should be rejected
        let result = manager.render_notification_email(
            "Test",
            "Test message",
            Some("Click"),
            Some("javascript:alert(1)"),
        );
        assert!(result.is_err());

        // data: scheme should be rejected
        let result = manager.render_notification_email(
            "Test",
            "Test message",
            Some("Click"),
            Some("data:text/html,<script>alert(1)</script>"),
        );
        assert!(result.is_err());

        // vbscript: scheme should be rejected
        let result = manager.render_notification_email(
            "Test",
            "Test message",
            Some("Click"),
            Some("vbscript:MsgBox"),
        );
        assert!(result.is_err());

        // Case-insensitive check
        let result = manager.render_notification_email(
            "Test",
            "Test message",
            Some("Click"),
            Some("JAVASCRIPT:alert(1)"),
        );
        assert!(result.is_err());

        // http:// should be allowed
        let result = manager.render_notification_email(
            "Test",
            "Test message",
            Some("Click"),
            Some("http://example.com"),
        );
        assert!(result.is_ok());

        // https:// should be allowed
        let result = manager.render_notification_email(
            "Test",
            "Test message",
            Some("Click"),
            Some("https://example.com"),
        );
        assert!(result.is_ok());

        // None action_url should be allowed
        let result = manager.render_notification_email("Test", "Test message", None, None);
        assert!(result.is_ok());
    }
}
