# Security Policy

## Supported Versions

SyncTV is currently developed on the main branch. Until stable release branches are published, security fixes target the current main branch and the latest published release, if one exists.

## Reporting a Vulnerability

Do not open a public issue with exploit details, tokens, credentials, logs containing secrets, or private user data.

Preferred reporting path:

1. Open a private GitHub Security Advisory for this repository if the feature is available.
2. If private advisories are not available, contact the maintainers through the repository first and ask for a private disclosure channel.
3. Include affected version or commit, deployment mode, impact, reproduction steps, and any relevant logs after removing secrets.

## Scope

Security reports may cover:

- Authentication, authorization, MFA, OAuth2, passkeys, token handling, and account recovery.
- Provider credentials, media proxying, request header handling, and SSRF-related behavior.
- HTTP, gRPC, WebSocket, management, metrics, and cluster control surfaces.
- Docker Compose, Helm, Kubernetes Ingress, and default deployment hardening.
- Protobuf/API design issues that can lead to privilege escalation, data exposure, or denial of service.

## Disclosure Expectations

Give maintainers reasonable time to investigate, patch, and publish guidance before public disclosure. Avoid sharing exploit code or live-service targets unless maintainers explicitly request controlled reproduction details.

## Handling Secrets

If a report includes accidental secrets, assume they are compromised. Rotate JWT secrets, OPAQUE setup secrets, provider tokens, OAuth2 client secrets, SMTP passwords, management tokens, and credential encryption keys according to the blast radius.
