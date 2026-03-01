//! Email templates for verification, password reset, and notifications
//!
//! Uses Handlebars for template rendering with variable substitution

use handlebars::Handlebars;
use serde_json::json;
use std::sync::Arc;

use crate::{Error, InternalExt, Result};

/// Email template type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailTemplateType {
    /// Email verification template
    EmailVerification,
    /// Password reset template
    PasswordReset,
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
    /// Create a new email template manager
    pub fn new() -> Result<Self> {
        let mut handlebars = Handlebars::new();

        // Register templates
        handlebars
            .register_template_string("email_verification", EMAIL_VERIFICATION_TEMPLATE)
            .internal_with_err("Failed to register email verification template")?;

        handlebars
            .register_template_string("password_reset", PASSWORD_RESET_TEMPLATE)
            .internal_with_err("Failed to register password reset template")?;

        handlebars
            .register_template_string("test_email", TEST_EMAIL_TEMPLATE)
            .internal_with_err("Failed to register test email template")?;

        handlebars
            .register_template_string("notification", NOTIFICATION_TEMPLATE)
            .internal_with_err("Failed to register notification template")?;

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

        let plain_text = format!(
            "Welcome to SyncTV!\n\n\
            Please verify your email address by entering the code below:\n\n\
            Verification Code: {token}\n\n\
            This code will expire in {expires_in}.\n\n\
            If you didn't create a SyncTV account, please ignore this email.\n\n\
            Best regards,\n\
            The SyncTV Team"
        );

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

        let plain_text = format!(
            "You requested a password reset for your SyncTV account.\n\n\
            Your password reset code is: {token}\n\n\
            This code will expire in {expires_in}.\n\n\
            If you didn't request a password reset, please ignore this email and your password will remain unchanged.\n\n\
            Best regards,\n\
            The SyncTV Team"
        );

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

        let plain_text = format!(
            "This is a test email from SyncTV.\n\n\
            If you received this email, your email configuration is working correctly.\n\n\
            SMTP Server: {smtp_host}:{smtp_port}\n\
            Sent at: {sent_at}\n\n\
            Best regards,\n\
            The SyncTV Team"
        );

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

        let plain_text = if let (Some(action_text), Some(action_url)) = (action_text, action_url) {
            format!(
                "{title}\n\n\
                {message}\n\n\
                {action_text}: {action_url}\n\n\
                Best regards,\n\
                The SyncTV Team"
            )
        } else {
            format!(
                "{title}\n\n\
                {message}\n\n\
                Best regards,\n\
                The SyncTV Team"
            )
        };

        Ok((html, plain_text))
    }
}

impl Default for EmailTemplateManager {
    fn default() -> Self {
        Self::new().expect("Failed to create default EmailTemplateManager")
    }
}

// Email verification template (HTML)
const EMAIL_VERIFICATION_TEMPLATE: &str = r#"
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Verify Your Email - SyncTV</title>
    <style>
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif;
            line-height: 1.6;
            color: #333;
            max-width: 600px;
            margin: 0 auto;
            padding: 20px;
            background-color: #f5f5f5;
        }
        .email-container {
            background-color: #ffffff;
            border-radius: 8px;
            padding: 40px;
            box-shadow: 0 2px 8px rgba(0,0,0,0.1);
        }
        .logo {
            text-align: center;
            margin-bottom: 30px;
        }
        .logo h1 {
            color: #4F46E5;
            margin: 0;
            font-size: 32px;
        }
        .content {
            margin-bottom: 30px;
        }
        h2 {
            color: #1F2937;
            margin-top: 0;
        }
        .verification-code {
            background-color: #EEF2FF;
            border: 2px dashed #4F46E5;
            border-radius: 8px;
            padding: 20px;
            text-align: center;
            margin: 30px 0;
        }
        .code {
            font-size: 32px;
            font-weight: bold;
            color: #4F46E5;
            letter-spacing: 8px;
            font-family: 'Courier New', monospace;
        }
        .expiry {
            color: #6B7280;
            font-size: 14px;
            margin-top: 10px;
        }
        .footer {
            text-align: center;
            color: #6B7280;
            font-size: 12px;
            margin-top: 40px;
            padding-top: 20px;
            border-top: 1px solid #E5E7EB;
        }
        .warning {
            background-color: #FEF3C7;
            border-left: 4px solid #F59E0B;
            padding: 12px;
            margin-top: 20px;
            font-size: 14px;
        }
    </style>
</head>
<body>
    <div class="email-container">
        <div class="logo">
            <h1>🎬 SyncTV</h1>
        </div>

        <div class="content">
            <h2>Welcome to SyncTV!</h2>
            <p>Thank you for creating an account. Please verify your email address to get started.</p>

            <div class="verification-code">
                <div class="code">{{token}}</div>
                <div class="expiry">This code will expire in {{expires_in}}</div>
            </div>

            <p>Enter this verification code in the app to complete your registration.</p>

            <div class="warning">
                <strong>Security Notice:</strong> If you didn't create a SyncTV account, please ignore this email. Your email address will not be used without verification.
            </div>
        </div>

        <div class="footer">
            <p>© SyncTV. All rights reserved.</p>
            <p>This is an automated message, please do not reply to this email.</p>
        </div>
    </div>
</body>
</html>
"#;

// Password reset template (HTML)
const PASSWORD_RESET_TEMPLATE: &str = r#"
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Reset Your Password - SyncTV</title>
    <style>
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif;
            line-height: 1.6;
            color: #333;
            max-width: 600px;
            margin: 0 auto;
            padding: 20px;
            background-color: #f5f5f5;
        }
        .email-container {
            background-color: #ffffff;
            border-radius: 8px;
            padding: 40px;
            box-shadow: 0 2px 8px rgba(0,0,0,0.1);
        }
        .logo {
            text-align: center;
            margin-bottom: 30px;
        }
        .logo h1 {
            color: #4F46E5;
            margin: 0;
            font-size: 32px;
        }
        .content {
            margin-bottom: 30px;
        }
        h2 {
            color: #1F2937;
            margin-top: 0;
        }
        .reset-code {
            background-color: #FEE2E2;
            border: 2px solid #EF4444;
            border-radius: 8px;
            padding: 20px;
            text-align: center;
            margin: 30px 0;
        }
        .code {
            font-size: 32px;
            font-weight: bold;
            color: #DC2626;
            letter-spacing: 8px;
            font-family: 'Courier New', monospace;
        }
        .expiry {
            color: #6B7280;
            font-size: 14px;
            margin-top: 10px;
        }
        .footer {
            text-align: center;
            color: #6B7280;
            font-size: 12px;
            margin-top: 40px;
            padding-top: 20px;
            border-top: 1px solid #E5E7EB;
        }
        .warning {
            background-color: #FEF3C7;
            border-left: 4px solid #F59E0B;
            padding: 12px;
            margin-top: 20px;
            font-size: 14px;
        }
    </style>
</head>
<body>
    <div class="email-container">
        <div class="logo">
            <h1>🎬 SyncTV</h1>
        </div>

        <div class="content">
            <h2>Password Reset Request</h2>
            <p>We received a request to reset your password. Use the code below to proceed:</p>

            <div class="reset-code">
                <div class="code">{{token}}</div>
                <div class="expiry">This code will expire in {{expires_in}}</div>
            </div>

            <p>Enter this code in the password reset form to create a new password.</p>

            <div class="warning">
                <strong>Security Notice:</strong> If you didn't request a password reset, please ignore this email. Your password will remain unchanged.
            </div>
        </div>

        <div class="footer">
            <p>© SyncTV. All rights reserved.</p>
            <p>This is an automated message, please do not reply to this email.</p>
        </div>
    </div>
</body>
</html>
"#;

// Test email template (HTML)
const TEST_EMAIL_TEMPLATE: &str = r#"
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Email Test - SyncTV</title>
    <style>
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif;
            line-height: 1.6;
            color: #333;
            max-width: 600px;
            margin: 0 auto;
            padding: 20px;
            background-color: #f5f5f5;
        }
        .email-container {
            background-color: #ffffff;
            border-radius: 8px;
            padding: 40px;
            box-shadow: 0 2px 8px rgba(0,0,0,0.1);
        }
        .logo {
            text-align: center;
            margin-bottom: 30px;
        }
        .logo h1 {
            color: #4F46E5;
            margin: 0;
            font-size: 32px;
        }
        .content {
            margin-bottom: 30px;
        }
        h2 {
            color: #1F2937;
            margin-top: 0;
        }
        .success-box {
            background-color: #D1FAE5;
            border: 2px solid #10B981;
            border-radius: 8px;
            padding: 20px;
            text-align: center;
            margin: 30px 0;
        }
        .success-icon {
            font-size: 48px;
            margin-bottom: 10px;
        }
        .success-text {
            color: #065F46;
            font-weight: bold;
            font-size: 18px;
        }
        .config-details {
            background-color: #F3F4F6;
            border-radius: 6px;
            padding: 15px;
            margin: 20px 0;
            font-family: 'Courier New', monospace;
            font-size: 14px;
        }
        .config-details dt {
            color: #6B7280;
            font-weight: normal;
            margin-top: 8px;
        }
        .config-details dd {
            color: #1F2937;
            font-weight: bold;
            margin-left: 0;
        }
        .footer {
            text-align: center;
            color: #6B7280;
            font-size: 12px;
            margin-top: 40px;
            padding-top: 20px;
            border-top: 1px solid #E5E7EB;
        }
    </style>
</head>
<body>
    <div class="email-container">
        <div class="logo">
            <h1>🎬 SyncTV</h1>
        </div>

        <div class="content">
            <h2>Email Configuration Test</h2>

            <div class="success-box">
                <div class="success-icon">✅</div>
                <div class="success-text">Email Configuration Working!</div>
            </div>

            <p>Congratulations! If you received this email, your email configuration is working correctly.</p>

            <div class="config-details">
                <dl>
                    <dt>SMTP Server:</dt>
                    <dd>{{smtp_host}}:{{smtp_port}}</dd>
                    <dt>Sent at:</dt>
                    <dd>{{sent_at}}</dd>
                </dl>
            </div>

            <p>You can now send emails for verification, password resets, and notifications.</p>
        </div>

        <div class="footer">
            <p>© SyncTV. All rights reserved.</p>
            <p>This is an automated message, please do not reply to this email.</p>
        </div>
    </div>
</body>
</html>
"#;

// Notification template (HTML)
const NOTIFICATION_TEMPLATE: &str = r#"
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{{title}} - SyncTV</title>
    <style>
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif;
            line-height: 1.6;
            color: #333;
            max-width: 600px;
            margin: 0 auto;
            padding: 20px;
            background-color: #f5f5f5;
        }
        .email-container {
            background-color: #ffffff;
            border-radius: 8px;
            padding: 40px;
            box-shadow: 0 2px 8px rgba(0,0,0,0.1);
        }
        .logo {
            text-align: center;
            margin-bottom: 30px;
        }
        .logo h1 {
            color: #4F46E5;
            margin: 0;
            font-size: 32px;
        }
        .content {
            margin-bottom: 30px;
        }
        h2 {
            color: #1F2937;
            margin-top: 0;
        }
        .message {
            background-color: #F9FAFB;
            border-left: 4px solid #4F46E5;
            padding: 16px;
            margin: 20px 0;
        }
        .button {
            display: inline-block;
            background-color: #4F46E5;
            color: #ffffff !important;
            text-decoration: none;
            padding: 12px 24px;
            border-radius: 6px;
            margin: 20px 0;
            font-weight: bold;
        }
        .button:hover {
            background-color: #4338CA;
        }
        .footer {
            text-align: center;
            color: #6B7280;
            font-size: 12px;
            margin-top: 40px;
            padding-top: 20px;
            border-top: 1px solid #E5E7EB;
        }
    </style>
</head>
<body>
    <div class="email-container">
        <div class="logo">
            <h1>🎬 SyncTV</h1>
        </div>

        <div class="content">
            <h2>{{title}}</h2>

            <div class="message">
                <p>{{message}}</p>
            </div>

            {{#if has_action}}
            <div style="text-align: center;">
                <a href="{{action_url}}" class="button">{{action_text}}</a>
            </div>
            {{/if}}
        </div>

        <div class="footer">
            <p>© SyncTV. All rights reserved.</p>
            <p>This is an automated message, please do not reply to this email.</p>
        </div>
    </div>
</body>
</html>
"#;

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
