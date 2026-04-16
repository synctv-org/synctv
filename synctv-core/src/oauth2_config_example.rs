//! OAuth2/OIDC Configuration Examples
//!
//! This file demonstrates how to configure multiple OAuth2/OIDC provider instances
//! via configuration files and environment variables.
//!
//! Configuration priority: Environment variables > Config file > Defaults
//!
//! Usage:
//! 1. Save this configuration in synctv.yaml under the oauth2 section
//! 2. Or set environment variables (SYNCTV_OAUTH2_PROVIDERS__<INSTANCE_ID>__*)

// Method 1: YAML Configuration File Example (synctv.yaml)

/*
oauth2:
  # GitHub instance (using default endpoints)
  github:
    type: "github"
    client_id: "your_github_client_id"
    client_secret: "your_github_client_secret"

  # Google instance
  google:
    type: "google"
    client_id: "your_google_client_id"
    client_secret: "your_google_client_secret"

  # Logto instance 1 (custom endpoint)
  logto1:
    type: "oidc"
    issuer: "https://logto1.your-domain.com"
    client_id: "logto1_client_id"
    client_secret: "logto1_client_secret"
    # scopes: ["openid", "profile", "email"]  # Optional, defaults to provider type's default scopes

  # Logto instance 2 (different Logto server)
  logto2:
    type: "oidc"
    issuer: "https://logto2.your-domain.com"
    client_id: "logto2_client_id"
    client_secret: "logto2_client_secret"

  # Casdoor instance
  casdoor_prod:
    type: "casdoor"
    endpoint: "https://casdoor.your-domain.com"
    client_id: "casdoor_client_id"
    client_secret: "casdoor_client_secret"

  # QQ instance
  qq:
    type: "qq"
    client_id: "qq_client_id"
    client_secret: "qq_client_secret"
    app_id: "your_qq_app_id"

  # Custom OIDC provider (OIDC server without .well-known support)
  custom_oidc:
    type: "oidc"
    issuer: "https://custom.oidc.provider.com"
    auth_url: "https://custom.oidc.provider.com/authorize"
    token_url: "https://custom.oidc.provider.com/token"
    userinfo_url: "https://custom.oidc.provider.com/userinfo"
    client_id: "custom_client_id"
    client_secret: "custom_client_secret"
*/

// Method 2: Environment Variable Configuration Example

/*
# General format: SYNCTV_OAUTH2_<INSTANCE_ID>__<FIELD>

# GitHub configuration
SYNCTV_OAUTH2_GITHUB__TYPE=github
SYNCTV_OAUTH2_GITHUB__CLIENT_ID=xxx
SYNCTV_OAUTH2_GITHUB__CLIENT_SECRET=yyy

# Logto instance 1
SYNCTV_OAUTH2_LOGTO1__TYPE=oidc
SYNCTV_OAUTH2_LOGTO1__ISSUER=https://logto1.your-domain.com
SYNCTV_OAUTH2_LOGTO1__CLIENT_ID=xxx
SYNCTV_OAUTH2_LOGTO1__CLIENT_SECRET=yyy

# Logto instance 2
SYNCTV_OAUTH2_LOGTO2__TYPE=oidc
SYNCTV_OAUTH2_LOGTO2__ISSUER=https://logto2.your-domain.com
SYNCTV_OAUTH2_LOGTO2__CLIENT_ID=aaa
SYNCTV_OAUTH2_LOGTO2__CLIENT_SECRET=bbb

# Casdoor configuration
SYNCTV_OAUTH2_CASDOOR__TYPE=casdoor
SYNCTV_OAUTH2_CASDOOR__ENDPOINT=https://casdoor.your-domain.com
SYNCTV_OAUTH2_CASDOOR__CLIENT_ID=xxx
SYNCTV_OAUTH2_CASDOOR__CLIENT_SECRET=yyy

# QQ configuration
SYNCTV_OAUTH2_QQ__TYPE=qq
SYNCTV_OAUTH2_QQ__CLIENT_ID=xxx
SYNCTV_OAUTH2_QQ__CLIENT_SECRET=yyy
SYNCTV_OAUTH2_QQ__APP_ID=your_qq_app_id

# Custom OIDC configuration (without .well-known support)
SYNCTV_OAUTH2_CUSTOM__TYPE=oidc
SYNCTV_OAUTH2_CUSTOM__ISSUER=https://custom.oidc.provider.com
SYNCTV_OAUTH2_CUSTOM__AUTH_URL=https://custom.oidc.provider.com/authorize
SYNCTV_OAUTH2_CUSTOM__TOKEN_URL=https://custom.oidc.provider.com/token
SYNCTV_OAUTH2_CUSTOM__USERINFO_URL=https://custom.oidc.provider.com/userinfo
SYNCTV_OAUTH2_CUSTOM__CLIENT_ID=xxx
SYNCTV_OAUTH2_CUSTOM__CLIENT_SECRET=yyy
*/

// Supported Provider Types

/*
Supported provider types:
  - github    GitHub OAuth2
  - google    Google OAuth2 + OIDC
  - microsoft Microsoft OAuth2 + OIDC
  - discord   Discord OAuth2
  - qq        QQ OAuth2
  - casdoor   Casdoor OIDC
  - logto     Logto OIDC
  - feishu    Feishu SSO
  - gitee     Gitee OAuth2
  - oidc      Generic OIDC provider

OIDC Provider configuration:
  1. If supports .well-known/openid-configuration (recommended):
     - Only configure issuer (will auto-discover other endpoints)
     - Example: issuer = "https://accounts.google.com"

  2. If does not support .well-known (or needs customization):
     - Configure auth_url, token_url, userinfo_url
     - See "custom_oidc" configuration below for example

Default scopes:
  - OIDC providers: ["openid", "profile", "email"]
  - OAuth2 providers: ["identify"]
  - Can override defaults via scopes field
*/

// Migration Guide: From Old Single Config to Multi-Instance Config

/*
Old configuration (not recommended):
  SYNCTV_OAUTH2_GITHUB__ENABLED=true
  SYNCTV_OAUTH2_GITHUB__CLIENT_ID=xxx
  SYNCTV_OAUTH2_GITHUB__CLIENT_SECRET=yyy

New configuration (recommended):
  # In synctv.yaml:
  oauth2:
    github:
      type: "github"
      client_id: "xxx"
      client_secret: "yyy"

Or using environment variables:
  SYNCTV_OAUTH2_GITHUB__TYPE=github
  SYNCTV_OAUTH2_GITHUB__CLIENT_ID=xxx
  SYNCTV_OAUTH2_GITHUB__CLIENT_SECRET=yyy
*/

fn main() {
    println!("OAuth2 Configuration Examples - Please refer to the comments above");
}
