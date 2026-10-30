# Remote MCP OAuth context

Date: 2026-10-10

_Anchors:_ `crates/project/src/context_server_store.rs::ContextServerStore`, `crates/context_server/src/oauth.rs::OAuthSession`, `crates/oauth_callback_server/src/oauth_callback_server.rs::OAuthCallbackServerConfig`, `crates/settings_content/src/project.rs::OAuthClientSettings`

- **Context server**: the configured MCP process or HTTP endpoint. The settings model lives in `crates/settings_content`; the effective configuration and lifecycle live in `crates/project`.
- **Registered redirect**: a provider-allowlisted loopback URI. The shared callback listener is in `crates/oauth_callback_server`; only selected HTTP context servers opt into an exact host/port.
- **OAuth session policy**: the per-server client ID and configured redirect/scope choices recorded with a keychain grant. A changed policy means the saved token cannot be reused; see `crates/context_server/src/oauth.rs::OAuthSessionPolicy`.
- **Scope fallback**: absent an explicit override, OAuth discovery chooses scopes from the challenge or protected-resource metadata. Configured scopes replace that list, including an explicitly empty list; see `crates/context_server/src/oauth.rs::select_scopes`.
