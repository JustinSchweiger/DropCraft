//! Konto-Website unter `/account` (Englisch und Deutsch, Standard Englisch).
//!
//! Für alle: Speicher, Passwort, angemeldete Geräte, eigene Links, Sprache.
//! Für Admins zusätzlich: Übersicht, Benutzer (anlegen, Speicher, Passwort,
//! Admin, sperren, abmelden, löschen) und Server-Einstellungen.
//!
//! Server-seitig gerendert, ohne JavaScript. Anmeldung per Formular oder per
//! Einmal-Code aus der App (`POST /api/account/web-login`). Die Sitzung ist
//! ein normaler Eintrag in `sessions` (taucht also unter „Geräte“ auf) und
//! steckt in einem HttpOnly-Cookie mit SameSite=Strict; POSTs prüfen zusätzlich
//! die Herkunft.

use std::collections::HashMap;

use axum::{
    extract::{Form, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    Json,
};
use rand::RngCore;
use serde::Deserialize;

use crate::{
    auth::{self, AuthUser},
    db::now_ms,
    error::ApiResult,
    settings::{self, Settings},
    shares::{date_time_l, esc, human_l, CSS, ICON_LOGO},
    AppState,
};

const COOKIE: &str = "dc_account";
const LANG_COOKIE: &str = "dc_lang";
const GB: f64 = 1024.0 * 1024.0 * 1024.0;

// ---------------------------------------------------------------- Sprache

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    De,
}

impl Lang {
    fn t<'a>(self, en: &'a str, de: &'a str) -> &'a str {
        if self == Lang::De {
            de
        } else {
            en
        }
    }

    fn code(self) -> &'static str {
        if self == Lang::De {
            "de"
        } else {
            "en"
        }
    }
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

/// Englisch, außer jemand hat Deutsch gewählt.
fn lang(headers: &HeaderMap) -> Lang {
    match cookie(headers, LANG_COOKIE).as_deref() {
        Some("de") => Lang::De,
        _ => Lang::En,
    }
}

// ---------------------------------------------------------------- Anmeldung

struct Viewer {
    id: i64,
    username: String,
    is_admin: bool,
    token_hash: String,
}

async fn viewer(state: &AppState, headers: &HeaderMap, s: &Settings) -> Option<Viewer> {
    let token = cookie(headers, COOKIE)?;
    let token_hash = auth::hash_token(&token);
    let row: (i64, String, bool, i64) = sqlx::query_as(
        "SELECT u.id, u.username, u.is_admin, s.last_used FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = ? AND u.disabled = 0",
    )
    .bind(&token_hash)
    .fetch_optional(&state.db)
    .await
    .ok()??;
    let now = now_ms();
    if now - row.3 > s.web_session_days as i64 * 86_400_000 {
        let _ = sqlx::query("DELETE FROM sessions WHERE token_hash = ?").bind(&token_hash).execute(&state.db).await;
        return None;
    }
    if now - row.3 > 600_000 {
        let _ = sqlx::query("UPDATE sessions SET last_used = ? WHERE token_hash = ?").bind(now).bind(&token_hash).execute(&state.db).await;
    }
    Some(Viewer { id: row.0, username: row.1, is_admin: row.2, token_hash })
}

/// Formulare nur von dieser Website (zusätzlich zu SameSite=Strict).
fn same_origin(headers: &HeaderMap) -> bool {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let from = headers
        .get(header::ORIGIN)
        .or_else(|| headers.get(header::REFERER))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let from_host = from.split("://").nth(1).unwrap_or("").split('/').next().unwrap_or("");
    !host.is_empty() && from_host.eq_ignore_ascii_case(host)
}

fn secure(headers: &HeaderMap) -> &'static str {
    let https = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()) == Some("https");
    if https {
        "; Secure"
    } else {
        ""
    }
}

fn session_cookie(headers: &HeaderMap, token: &str, days: u32) -> String {
    format!("{COOKIE}={token}; Path=/account; HttpOnly; SameSite=Strict; Max-Age={}{}", days as u64 * 86_400, secure(headers))
}

fn device_name(headers: &HeaderMap) -> String {
    let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
    format!("{} · {}", crate::shares::device_label(ua), "Account website")
}

async fn start_session(state: &AppState, headers: &HeaderMap, user_id: i64, to: &str) -> Response {
    let s = settings::load(state).await;
    match auth::create_session(state, user_id, &device_name(headers)).await {
        Ok(token) => {
            let mut r = Redirect::to(to).into_response();
            r.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&session_cookie(headers, &token, s.web_session_days)).unwrap());
            r
        }
        Err(e) => e.into_response(),
    }
}

/// Einmal-Codes aus der App: Hash -> (Benutzer, gültig bis).
pub type LoginCodes = std::sync::Arc<std::sync::Mutex<HashMap<String, (i64, i64)>>>;

#[derive(Deserialize)]
pub struct WebLoginReq {
    #[serde(default)]
    page: Option<String>,
}

/// `POST /api/account/web-login` → `{ "url": "/account/auth?code=…" }`:
/// öffnet die Konto-Website aus der App heraus, ohne neue Anmeldung.
pub async fn web_login(State(state): State<AppState>, user: AuthUser, body: Option<Json<WebLoginReq>>) -> ApiResult<Json<serde_json::Value>> {
    let mut raw = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let code = hex::encode(raw);
    let now = now_ms();
    {
        let mut codes = state.login_codes.lock().unwrap();
        codes.retain(|_, (_, exp)| *exp > now);
        codes.insert(auth::hash_token(&code), (user.id, now + 120_000));
    }
    let page = match body.and_then(|b| b.0.page).as_deref() {
        Some("admin") if user.is_admin => "admin",
        _ => "",
    };
    Ok(Json(serde_json::json!({ "url": format!("/account/auth?code={code}&page={page}") })))
}

#[derive(Deserialize)]
pub struct AuthQuery {
    code: String,
    #[serde(default)]
    page: String,
}

pub async fn auth_code(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<AuthQuery>) -> Response {
    let entry = state.login_codes.lock().unwrap().remove(&auth::hash_token(&q.code));
    match entry {
        Some((user_id, exp)) if exp > now_ms() => {
            let to = if q.page == "admin" { "/account/admin" } else { "/account" };
            start_session(&state, &headers, user_id, to).await
        }
        _ => Redirect::to("/account?m=code").into_response(),
    }
}

#[derive(Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
}

pub async fn login(State(state): State<AppState>, headers: HeaderMap, Form(f): Form<LoginForm>) -> Response {
    if !same_origin(&headers) {
        return forbidden(lang(&headers));
    }
    match auth::check_login(&state, f.username.trim(), &f.password).await {
        Ok(id) => start_session(&state, &headers, id, "/account").await,
        Err(crate::error::ApiError::Forbidden) => Redirect::to("/account?m=disabled").into_response(),
        Err(_) => Redirect::to("/account?m=login").into_response(),
    }
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !same_origin(&headers) {
        return forbidden(lang(&headers));
    }
    if let Some(t) = cookie(&headers, COOKIE) {
        let _ = sqlx::query("DELETE FROM sessions WHERE token_hash = ?").bind(auth::hash_token(&t)).execute(&state.db).await;
    }
    let mut r = Redirect::to("/account?m=bye").into_response();
    r.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&format!("{COOKIE}=; Path=/account; HttpOnly; SameSite=Strict; Max-Age=0{}", secure(&headers))).unwrap());
    r
}

#[derive(Deserialize)]
pub struct LangForm {
    lang: String,
    #[serde(default)]
    back: String,
}

pub async fn set_lang(headers: HeaderMap, Form(f): Form<LangForm>) -> Response {
    let code = if f.lang == "de" { "de" } else { "en" };
    let back = if f.back.starts_with("/account") && !f.back.starts_with("//") { f.back } else { "/account".into() };
    let mut r = Redirect::to(&back).into_response();
    r.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!("{LANG_COOKIE}={code}; Path=/; SameSite=Lax; Max-Age=31536000{}", secure(&headers))).unwrap(),
    );
    r
}

// ---------------------------------------------------------------- Seitenrahmen

#[derive(Deserialize, Default)]
pub struct Msg {
    #[serde(default)]
    m: Option<String>,
}

/// Rückmeldung nach einer Aktion (`?m=…`): (erfolgreich?, Text).
fn flash(l: Lang, m: &Option<String>) -> String {
    let Some(code) = m.as_deref() else { return String::new() };
    let (ok, text) = match code {
        "pw" => (true, l.t("Password changed. Your other devices were signed out.", "Passwort geändert. Deine anderen Geräte wurden abgemeldet.")),
        "pw_wrong" => (false, l.t("The current password is wrong.", "Das aktuelle Passwort stimmt nicht.")),
        "pw_match" => (false, l.t("The new passwords do not match.", "Die neuen Passwörter stimmen nicht überein.")),
        "pw_short" => (false, l.t("The password is too short.", "Das Passwort ist zu kurz.")),
        "dev" => (true, l.t("Device signed out.", "Gerät abgemeldet.")),
        "others" => (true, l.t("All other devices were signed out.", "Alle anderen Geräte wurden abgemeldet.")),
        "link" => (true, l.t("Link ended. It no longer works.", "Link beendet. Er funktioniert nicht mehr.")),
        "login" => (false, l.t("Wrong username or password.", "Benutzername oder Passwort falsch.")),
        "disabled" => (false, l.t("This account is disabled.", "Dieses Konto ist gesperrt.")),
        "code" => (false, l.t("This sign-in link has expired. Please sign in.", "Dieser Anmelde-Link ist abgelaufen. Bitte melde dich an.")),
        "bye" => (true, l.t("You are signed out.", "Du bist abgemeldet.")),
        "created" => (true, l.t("User created.", "Benutzer angelegt.")),
        "saved" => (true, l.t("Saved.", "Gespeichert.")),
        "deleted" => (true, l.t("User deleted with all files.", "Benutzer samt allen Dateien gelöscht.")),
        "signedout" => (true, l.t("The user was signed out on all devices.", "Der Benutzer wurde auf allen Geräten abgemeldet.")),
        "taken" => (false, l.t("This username is taken.", "Dieser Benutzername ist vergeben.")),
        "name" => (false, l.t("Username: 3–32 characters, only letters, digits, . _ -", "Benutzername: 3–32 Zeichen, nur Buchstaben, Zahlen, . _ -")),
        "quota" => (false, l.t("Please enter the storage in GB, e.g. 50 or 2.5.", "Bitte den Speicher in GB angeben, z. B. 50 oder 2,5.")),
        "self" => (false, l.t("You cannot do this to your own account.", "Das geht nicht mit deinem eigenen Konto.")),
        "confirm" => (false, l.t("Type the username to confirm.", "Zum Bestätigen den Benutzernamen eintippen.")),
        "number" => (false, l.t("Please check the numbers.", "Bitte die Zahlen prüfen.")),
        _ => return String::new(),
    };
    format!(r#"<div class="flash {}">{}</div>"#, if ok { "ok" } else { "bad" }, esc(text))
}

fn forbidden(l: Lang) -> Response {
    let mut r = Html(l.t("Forbidden", "Nicht erlaubt").to_string()).into_response();
    *r.status_mut() = StatusCode::FORBIDDEN;
    r
}

/// Ganze Seite mit Kopfzeile (Navigation, Sprache, Abmelden).
fn page(l: Lang, s: &Settings, v: Option<&Viewer>, here: &str, title: &str, main: &str) -> Response {
    let nav = match v {
        Some(v) => {
            let tab = |href: &str, label: &str, on: bool| format!(r#"<a class="tab{}" href="{href}">{}</a>"#, if on { " on" } else { "" }, esc(label));
            let admin = if v.is_admin { tab("/account/admin", l.t("Admin", "Verwaltung"), here.starts_with("/account/admin")) } else { String::new() };
            format!(
                r#"<nav class="tabs">{}{admin}<a class="tab" href="/app/">{}</a></nav>
<form method="post" action="/account/logout" class="inline"><button class="btn ghost" type="submit">{}</button></form>"#,
                tab("/account", l.t("Account", "Konto"), !here.starts_with("/account/admin")),
                l.t("Files", "Dateien"),
                l.t("Sign out", "Abmelden"),
            )
        }
        None => String::new(),
    };
    let other = if l == Lang::De { ("en", "English") } else { ("de", "Deutsch") };
    let switch = format!(
        r#"<form method="post" action="/account/lang" class="inline"><input type="hidden" name="lang" value="{}"><input type="hidden" name="back" value="{}"><button class="btn ghost" type="submit">{}</button></form>"#,
        other.0,
        esc(here),
        other.1
    );
    let html = format!(
        r#"<!doctype html><html lang="{}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<meta name="robots" content="noindex"><title>{} · {}</title><style>{CSS}{ACCOUNT_CSS}</style></head>
<body><header><a class="brand" href="/">{ICON_LOGO}<span>{}</span></a><div class="head-right">{nav}{switch}</div></header>
<main>{main}</main></body></html>"#,
        l.code(),
        esc(title),
        esc(&s.site_name),
        esc(&s.site_name.to_uppercase()),
    );
    let mut resp = Html(html).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"),
    );
    // „same-origin“ statt „no-referrer“: sonst schicken Browser bei Formularen
    // `Origin: null`, und die Herkunftsprüfung lehnt sie ab.
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn bar(used: i64, quota: i64) -> String {
    let pct = if quota > 0 { (used as f64 / quota as f64 * 100.0).clamp(0.0, 100.0) } else { 0.0 };
    format!(r#"<div class="meter"><div class="meter-fill{}" style="width:{pct:.1}%"></div></div>"#, if pct > 90.0 { " warn" } else { "" })
}

fn gb_text(l: Lang, bytes: i64) -> String {
    let g = bytes as f64 / GB;
    let s = if (g - g.round()).abs() < 0.05 { format!("{}", g.round() as i64) } else { format!("{g:.1}") };
    if l == Lang::De {
        s.replace('.', ",")
    } else {
        s
    }
}

fn parse_gb(v: &str) -> Option<f64> {
    v.trim().replace(',', ".").parse::<f64>().ok().filter(|g| *g >= 0.0 && g.is_finite())
}

// ---------------------------------------------------------------- Konto

pub async fn home(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<Msg>) -> Response {
    let l = lang(&headers);
    let s = settings::load(&state).await;
    let Some(v) = viewer(&state, &headers, &s).await else { return login_page(l, &s, &q) };

    let (quota, used): (i64, i64) = sqlx::query_as("SELECT quota_bytes, used_bytes FROM users WHERE id = ?")
        .bind(v.id)
        .fetch_one(&state.db)
        .await
        .unwrap_or((0, 0));

    // Geräte
    let devices: Vec<(i64, String, i64, i64, bool)> = sqlx::query_as(
        "SELECT rowid, device_name, created_at, last_used, token_hash = ? FROM sessions WHERE user_id = ? ORDER BY token_hash = ? DESC, last_used DESC",
    )
    .bind(&v.token_hash)
    .bind(v.id)
    .bind(&v.token_hash)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let mut dev_rows = String::new();
    for (id, name, created, last, current) in &devices {
        let action = if *current {
            format!(r#"<span class="chip on">{}</span>"#, l.t("This browser", "Dieser Browser"))
        } else {
            format!(
                r#"<form method="post" action="/account/devices/{id}/signout" class="inline"><button class="btn small" type="submit">{}</button></form>"#,
                l.t("Sign out", "Abmelden")
            )
        };
        dev_rows.push_str(&format!(
            r#"<li class="item"><div class="txt"><span class="name">{}</span><span class="meta">{} {} · {} {}</span></div>{action}</li>"#,
            esc(name),
            l.t("Signed in", "Angemeldet"),
            date_time_l(l == Lang::De, *created),
            l.t("last active", "zuletzt aktiv"),
            date_time_l(l == Lang::De, *last),
        ));
    }
    let others = if devices.len() > 1 {
        format!(
            r#"<form method="post" action="/account/devices/signout-others"><button class="btn" type="submit">{}</button></form>"#,
            l.t("Sign out all other devices", "Alle anderen Geräte abmelden")
        )
    } else {
        String::new()
    };

    // Links
    type LinkRow = (String, String, i64, bool, bool, Option<i64>);
    let links: Vec<LinkRow> = sqlx::query_as(
        "SELECT token, path, created_at, allow_upload, password_hash IS NOT NULL, expires_at FROM shares WHERE user_id = ? ORDER BY created_at DESC",
    )
    .bind(v.id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let now = now_ms();
    let mut link_rows = String::new();
    for (token, path, created, upload, pw, expires) in &links {
        let mut chips = Vec::new();
        if *upload {
            chips.push(l.t("Upload", "Upload").to_string());
        }
        if *pw {
            chips.push(l.t("Password", "Passwort").to_string());
        }
        match expires {
            Some(t) if *t < now => chips.push(l.t("Expired", "Abgelaufen").to_string()),
            Some(t) => chips.push(format!("{} {}", l.t("until", "bis"), date_time_l(l == Lang::De, *t))),
            None => {}
        }
        let chips: String = chips.iter().map(|c| format!(r#"<span class="chip">{}</span>"#, esc(c))).collect();
        link_rows.push_str(&format!(
            r#"<li class="item"><div class="txt"><a class="name link" href="/s/{token}" target="_blank" rel="noopener">{}</a><span class="meta">{} {} {chips}</span></div>
<form method="post" action="/account/links/{token}/end" class="inline"><button class="btn small" type="submit">{}</button></form></li>"#,
            esc(path),
            l.t("Created", "Erstellt"),
            date_time_l(l == Lang::De, *created),
            l.t("End", "Beenden"),
        ));
    }
    let links_html = if links.is_empty() {
        format!(r#"<p class="empty">{}</p>"#, l.t("You have no share links. Create them in the app or in the Explorer menu.", "Du hast keine Links. Du erstellst sie in der App oder im Explorer-Menü."))
    } else {
        format!(r#"<ul class="card list2">{link_rows}</ul>"#)
    };

    let admin_hint = if v.is_admin {
        format!(r#"<a class="btn primary" href="/account/admin">{}</a>"#, l.t("Open administration", "Verwaltung öffnen"))
    } else {
        String::new()
    };
    let min = s.min_password_len;
    let body = format!(
        r#"{flash}
<div class="title-row"><h1>{name}</h1>{badge}{admin_hint}</div>
<section class="card pad"><div class="label">{storage}</div><div class="big-num">{used} <span class="meta">{of} {quota} GB</span></div>{meter}</section>

<h2>{devices_t}</h2>
<p class="hint">{devices_hint}</p>
<ul class="card list2">{dev_rows}</ul>{others}

<h2>{links_t}</h2>
{links_html}

<h2>{pw_t}</h2>
<form method="post" action="/account/password" class="card pad form">
<label>{cur}<input class="input" type="password" name="old" autocomplete="current-password" required></label>
<label>{new} <span class="meta">({min_t})</span><input class="input" type="password" name="new" minlength="{min}" autocomplete="new-password" required></label>
<label>{rep}<input class="input" type="password" name="new2" minlength="{min}" autocomplete="new-password" required></label>
<div><button class="btn primary" type="submit">{change}</button></div>
</form>"#,
        flash = flash(l, &q.m),
        name = esc(&v.username),
        badge = if v.is_admin { format!(r#"<span class="chip on">{}</span>"#, l.t("Admin", "Admin")) } else { String::new() },
        storage = l.t("Storage", "Speicher"),
        used = human_l(l == Lang::De, used),
        of = l.t("of", "von"),
        quota = gb_text(l, quota),
        meter = bar(used, quota),
        devices_t = l.t("Signed-in devices", "Angemeldete Geräte"),
        devices_hint = l.t("Apps, browsers and this website. Signing out a device stops it from syncing until you sign in there again.", "Apps, Browser und diese Website. Ein abgemeldetes Gerät synchronisiert erst wieder, wenn du dich dort neu anmeldest."),
        links_t = l.t("Share links", "Freigabe-Links"),
        pw_t = l.t("Change password", "Passwort ändern"),
        cur = l.t("Current password", "Aktuelles Passwort"),
        new = l.t("New password", "Neues Passwort"),
        min_t = format!("{} {min} {}", l.t("at least", "mind."), l.t("characters", "Zeichen")),
        rep = l.t("Repeat new password", "Neues Passwort wiederholen"),
        change = l.t("Change password", "Passwort ändern"),
    );
    page(l, &s, Some(&v), "/account", l.t("Account", "Konto"), &body)
}

fn login_page(l: Lang, s: &Settings, q: &Msg) -> Response {
    let body = format!(
        r#"<section class="hero small"><div class="hero-logo">{logo}</div><h1>{title}</h1><p class="lead">{lead}</p></section>
{flash}
<form method="post" action="/account/login" class="card pad form narrow">
<label>{user}<input class="input" name="username" autocomplete="username" autocapitalize="none" required autofocus></label>
<label>{pw}<input class="input" type="password" name="password" autocomplete="current-password" required></label>
<div><button class="btn primary" type="submit">{signin}</button></div>
</form>
<p class="hint center">{hint}</p>"#,
        logo = crate::shares::ICON_LOGO_BIG,
        title = l.t("Your account", "Dein Konto"),
        lead = esc(&format!("{} · {}", s.site_name, l.t("storage, devices, links and administration", "Speicher, Geräte, Links und Verwaltung"))),
        flash = flash(l, &q.m),
        user = l.t("Username", "Benutzername"),
        pw = l.t("Password", "Passwort"),
        signin = l.t("Sign in", "Anmelden"),
        hint = l.t("Tip: in the DropCraft app, “Manage account” opens this page already signed in.", "Tipp: In der DropCraft-App öffnet „Konto verwalten“ diese Seite gleich angemeldet."),
    );
    page(l, s, None, "/account", l.t("Sign in", "Anmelden"), &body)
}

#[derive(Deserialize)]
pub struct PasswordForm {
    old: String,
    new: String,
    new2: String,
}

pub async fn change_password(State(state): State<AppState>, headers: HeaderMap, Form(f): Form<PasswordForm>) -> Response {
    let s = settings::load(&state).await;
    let Some(v) = guard(&state, &headers, &s).await else { return forbidden(lang(&headers)) };
    if f.new != f.new2 {
        return Redirect::to("/account?m=pw_match").into_response();
    }
    if f.new.chars().count() < s.min_password_len as usize {
        return Redirect::to("/account?m=pw_short").into_response();
    }
    match auth::check_login(&state, &v.username, &f.old).await {
        Ok(_) => {}
        Err(_) => return Redirect::to("/account?m=pw_wrong").into_response(),
    }
    if auth::set_password(&state, v.id, &f.new, Some(&v.token_hash)).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    Redirect::to("/account?m=pw").into_response()
}

/// Angemeldet und Formular von dieser Website?
async fn guard(state: &AppState, headers: &HeaderMap, s: &Settings) -> Option<Viewer> {
    if !same_origin(headers) {
        return None;
    }
    viewer(state, headers, s).await
}

pub async fn signout_device(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<i64>) -> Response {
    let s = settings::load(&state).await;
    let Some(v) = guard(&state, &headers, &s).await else { return forbidden(lang(&headers)) };
    let _ = sqlx::query("DELETE FROM sessions WHERE rowid = ? AND user_id = ? AND token_hash != ?")
        .bind(id)
        .bind(v.id)
        .bind(&v.token_hash)
        .execute(&state.db)
        .await;
    Redirect::to("/account?m=dev").into_response()
}

pub async fn signout_others(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let s = settings::load(&state).await;
    let Some(v) = guard(&state, &headers, &s).await else { return forbidden(lang(&headers)) };
    let _ = sqlx::query("DELETE FROM sessions WHERE user_id = ? AND token_hash != ?").bind(v.id).bind(&v.token_hash).execute(&state.db).await;
    Redirect::to("/account?m=others").into_response()
}

pub async fn end_link(State(state): State<AppState>, headers: HeaderMap, Path(token): Path<String>) -> Response {
    let s = settings::load(&state).await;
    let Some(v) = guard(&state, &headers, &s).await else { return forbidden(lang(&headers)) };
    let _ = sqlx::query("DELETE FROM shares WHERE token = ? AND user_id = ?").bind(&token).bind(v.id).execute(&state.db).await;
    Redirect::to("/account?m=link").into_response()
}

// ---------------------------------------------------------------- Verwaltung

async fn admin_viewer(state: &AppState, headers: &HeaderMap, s: &Settings) -> Option<Viewer> {
    viewer(state, headers, s).await.filter(|v| v.is_admin)
}

type UserRow = (i64, String, bool, bool, i64, i64, i64, i64, Option<i64>);
const USER_COLS: &str = "u.id, u.username, u.is_admin, u.disabled, u.quota_bytes, u.used_bytes, u.created_at, \
    (SELECT COUNT(*) FROM sessions s WHERE s.user_id = u.id), (SELECT MAX(last_used) FROM sessions s WHERE s.user_id = u.id)";

pub async fn admin_home(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<Msg>) -> Response {
    let l = lang(&headers);
    let s = settings::load(&state).await;
    let Some(v) = admin_viewer(&state, &headers, &s).await else { return Redirect::to("/account").into_response() };
    let de = l == Lang::De;

    let (n_users, used, quota): (i64, i64, i64) = sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(used_bytes), 0), COALESCE(SUM(quota_bytes), 0) FROM users")
        .fetch_one(&state.db)
        .await
        .unwrap_or((0, 0, 0));
    let (n_files,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM files WHERE deleted = 0 AND is_dir = 0").fetch_one(&state.db).await.unwrap_or((0,));
    let (n_links,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM shares").fetch_one(&state.db).await.unwrap_or((0,));
    let (n_dev,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions").fetch_one(&state.db).await.unwrap_or((0,));
    let disk = disk_free(&state.cfg.data_dir);

    let users: Vec<UserRow> = sqlx::query_as(&format!("SELECT {USER_COLS} FROM users u ORDER BY u.username COLLATE NOCASE"))
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let mut rows = String::new();
    for (id, name, is_admin, disabled, q_bytes, u_bytes, _created, n_sess, last) in &users {
        let mut chips = String::new();
        if *is_admin {
            chips.push_str(&format!(r#"<span class="chip on">{}</span>"#, l.t("Admin", "Admin")));
        }
        if *disabled {
            chips.push_str(&format!(r#"<span class="chip bad">{}</span>"#, l.t("Disabled", "Gesperrt")));
        }
        let seen = match last {
            Some(t) => format!("{} {}", l.t("last active", "zuletzt aktiv"), date_time_l(de, *t)),
            None => l.t("never signed in", "nie angemeldet").to_string(),
        };
        rows.push_str(&format!(
            r#"<li><a class="item link-row" href="/account/admin/users/{id}"><div class="txt"><span class="name">{} {chips}</span>
<span class="meta">{} / {} GB · {n_sess} {} · {seen}</span>{}</div><span class="chev">›</span></a></li>"#,
            esc(name),
            human_l(de, *u_bytes),
            gb_text(l, *q_bytes),
            if *n_sess == 1 { l.t("device", "Gerät") } else { l.t("devices", "Geräte") },
            bar(*u_bytes, *q_bytes),
        ));
    }

    let stat = |label: &str, value: String| format!(r#"<div class="stat"><div class="meta">{}</div><div class="big-num">{}</div></div>"#, esc(label), value);
    let stats = [
        stat(l.t("Users", "Benutzer"), n_users.to_string()),
        stat(l.t("Storage used", "Belegt"), format!("{} <span class=\"meta\">/ {} GB {}</span>", human_l(de, used), gb_text(l, quota), l.t("assigned", "vergeben"))),
        stat(l.t("Files", "Dateien"), n_files.to_string()),
        stat(l.t("Share links", "Links"), n_links.to_string()),
        stat(l.t("Signed-in devices", "Angemeldete Geräte"), n_dev.to_string()),
        stat(l.t("Free disk space", "Freier Plattenplatz"), disk.map(|b| human_l(de, b)).unwrap_or_else(|| "–".into())),
    ]
    .concat();

    let min = s.min_password_len;
    let body = format!(
        r#"{flash}
<div class="title-row"><h1>{title}</h1><a class="btn" href="/account/admin/settings">{settings_t}</a></div>
<div class="stats">{stats}</div>
<h2>{users_t}</h2>
<ul class="card list2">{rows}</ul>
<h2>{new_t}</h2>
<form method="post" action="/account/admin/users" class="card pad form">
<div class="grid2">
<label>{user_t}<input class="input" name="username" autocapitalize="none" required></label>
<label>{quota_t}<input class="input" name="quota" value="{dq}" inputmode="decimal" required></label>
</div>
<label>{pw_t} <span class="meta">({min_t})</span><input class="input" type="password" name="password" minlength="{min}" autocomplete="new-password" required></label>
<label class="check"><input type="checkbox" name="is_admin" value="1"> {admin_t}</label>
<div><button class="btn primary" type="submit">{create_t}</button></div>
</form>"#,
        flash = flash(l, &q.m),
        title = l.t("Administration", "Verwaltung"),
        settings_t = l.t("Server settings", "Server-Einstellungen"),
        users_t = l.t("Users", "Benutzer"),
        new_t = l.t("New user", "Neuer Benutzer"),
        user_t = l.t("Username", "Benutzername"),
        quota_t = l.t("Storage in GB", "Speicher in GB"),
        dq = gb_text(l, s.default_quota_bytes()),
        pw_t = l.t("Password", "Passwort"),
        min_t = format!("{} {min} {}", l.t("at least", "mind."), l.t("characters", "Zeichen")),
        admin_t = l.t("Administrator (can manage users and settings)", "Administrator (darf Benutzer und Einstellungen verwalten)"),
        create_t = l.t("Create user", "Benutzer anlegen"),
    );
    page(l, &s, Some(&v), "/account/admin", l.t("Administration", "Verwaltung"), &body)
}

fn disk_free(path: &std::path::Path) -> Option<i64> {
    #[cfg(unix)]
    {
        let out = std::process::Command::new("df").arg("-Pk").arg(path).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.lines().nth(1)?;
        let kb: i64 = line.split_whitespace().nth(3)?.parse().ok()?;
        Some(kb * 1024)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

#[derive(Deserialize)]
pub struct NewUserForm {
    username: String,
    password: String,
    quota: String,
    #[serde(default)]
    is_admin: Option<String>,
}

pub async fn admin_create(State(state): State<AppState>, headers: HeaderMap, Form(f): Form<NewUserForm>) -> Response {
    let s = settings::load(&state).await;
    if guard(&state, &headers, &s).await.filter(|v| v.is_admin).is_none() {
        return forbidden(lang(&headers));
    }
    let name = f.username.trim();
    if auth::validate_username(name).is_err() {
        return Redirect::to("/account/admin?m=name").into_response();
    }
    if f.password.chars().count() < s.min_password_len as usize {
        return Redirect::to("/account/admin?m=pw_short").into_response();
    }
    let Some(gb) = parse_gb(&f.quota) else { return Redirect::to("/account/admin?m=quota").into_response() };
    match auth::create_user(&state, name, &f.password, (gb * GB) as i64, f.is_admin.is_some()).await {
        Ok(_) => Redirect::to("/account/admin?m=created").into_response(),
        Err(crate::error::ApiError::Conflict(_)) => Redirect::to("/account/admin?m=taken").into_response(),
        Err(e) => e.into_response(),
    }
}

pub async fn admin_user(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<i64>, Query(q): Query<Msg>) -> Response {
    let l = lang(&headers);
    let s = settings::load(&state).await;
    let Some(v) = admin_viewer(&state, &headers, &s).await else { return Redirect::to("/account").into_response() };
    let de = l == Lang::De;
    let row: Option<UserRow> = sqlx::query_as(&format!("SELECT {USER_COLS} FROM users u WHERE u.id = ?")).bind(id).fetch_optional(&state.db).await.unwrap_or(None);
    let Some((id, name, is_admin, disabled, q_bytes, u_bytes, created, _n, _last)) = row else { return Redirect::to("/account/admin").into_response() };
    let me = id == v.id;

    let devices: Vec<(String, i64, i64)> = sqlx::query_as("SELECT device_name, created_at, last_used FROM sessions WHERE user_id = ? ORDER BY last_used DESC")
        .bind(id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let dev_rows: String = devices
        .iter()
        .map(|(d, c, last)| {
            format!(
                r#"<li class="item"><div class="txt"><span class="name">{}</span><span class="meta">{} {} · {} {}</span></div></li>"#,
                esc(d),
                l.t("Signed in", "Angemeldet"),
                date_time_l(de, *c),
                l.t("last active", "zuletzt aktiv"),
                date_time_l(de, *last)
            )
        })
        .collect();
    let dev_html = if devices.is_empty() {
        format!(r#"<p class="empty">{}</p>"#, l.t("Not signed in anywhere.", "Nirgends angemeldet."))
    } else {
        format!(
            r#"<ul class="card list2">{dev_rows}</ul><form method="post" action="/account/admin/users/{id}/signout"><button class="btn" type="submit">{}</button></form>"#,
            l.t("Sign out on all devices", "Auf allen Geräten abmelden")
        )
    };
    let (n_files,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM files WHERE user_id = ? AND deleted = 0 AND is_dir = 0").bind(id).fetch_one(&state.db).await.unwrap_or((0,));
    let (n_links,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM shares WHERE user_id = ?").bind(id).fetch_one(&state.db).await.unwrap_or((0,));
    let lock = if me { " disabled" } else { "" };
    let checked = |b: bool| if b { " checked" } else { "" };
    let min = s.min_password_len;
    let danger = if me {
        String::new()
    } else {
        format!(
            r#"<h2>{del_t}</h2>
<form method="post" action="/account/admin/users/{id}/delete" class="card pad form danger-zone">
<p>{del_hint}</p>
<label>{confirm_t}<input class="input" name="confirm" autocapitalize="none" autocomplete="off" required></label>
<div><button class="btn danger" type="submit">{del_btn}</button></div>
</form>"#,
            del_t = l.t("Delete user", "Benutzer löschen"),
            del_hint = esc(&format!(
                "{} {} {}",
                l.t("Deletes the account and all", "Löscht das Konto und alle"),
                n_files,
                l.t("files on the server. This cannot be undone.", "Dateien auf dem Server. Das lässt sich nicht rückgängig machen.")
            )),
            confirm_t = esc(&format!("{} „{}“", l.t("Type the username to confirm:", "Zum Bestätigen den Benutzernamen eintippen:"), name)),
            del_btn = l.t("Delete user and files", "Benutzer und Dateien löschen"),
        )
    };
    let body = format!(
        r#"{flash}
<p class="crumbs"><a href="/account/admin">{admin_t}</a><span class="sep">›</span>{name_e}</p>
<div class="title-row"><h1>{name_e}</h1>{badges}</div>
<section class="card pad"><div class="label">{storage_t}</div><div class="big-num">{used} <span class="meta">{of} {quota} GB · {n_files} {files_t} · {n_links} {links_t} · {since_t} {created}</span></div>{meter}</section>

<h2>{edit_t}</h2>
<form method="post" action="/account/admin/users/{id}" class="card pad form">
<label>{quota_t}<input class="input" name="quota" value="{quota}" inputmode="decimal" required></label>
<label>{pw_t} <span class="meta">({pw_hint})</span><input class="input" type="password" name="password" minlength="{min}" autocomplete="new-password"></label>
<label class="check"><input type="checkbox" name="is_admin" value="1"{adm}{lock}> {admin_c}</label>
<label class="check"><input type="checkbox" name="disabled" value="1"{dis}{lock}> {disabled_c}</label>
<div><button class="btn primary" type="submit">{save_t}</button></div>
</form>

<h2>{devices_t}</h2>
{dev_html}
{danger}"#,
        flash = flash(l, &q.m),
        admin_t = l.t("Administration", "Verwaltung"),
        name_e = esc(&name),
        badges = format!(
            "{}{}",
            if is_admin { format!(r#"<span class="chip on">{}</span>"#, l.t("Admin", "Admin")) } else { String::new() },
            if disabled { format!(r#"<span class="chip bad">{}</span>"#, l.t("Disabled", "Gesperrt")) } else { String::new() }
        ),
        storage_t = l.t("Storage", "Speicher"),
        used = human_l(de, u_bytes),
        of = l.t("of", "von"),
        quota = gb_text(l, q_bytes),
        files_t = l.t("files", "Dateien"),
        links_t = l.t("links", "Links"),
        since_t = l.t("since", "seit"),
        created = date_time_l(de, created),
        meter = bar(u_bytes, q_bytes),
        edit_t = l.t("Settings", "Einstellungen"),
        quota_t = l.t("Storage in GB", "Speicher in GB"),
        pw_t = l.t("New password", "Neues Passwort"),
        pw_hint = l.t("leave empty to keep it; setting one signs the user out everywhere", "leer lassen = unverändert; ein neues meldet überall ab"),
        adm = checked(is_admin),
        dis = checked(disabled),
        admin_c = l.t("Administrator", "Administrator"),
        disabled_c = l.t("Disabled (cannot sign in, apps stop syncing)", "Gesperrt (keine Anmeldung, Apps synchronisieren nicht mehr)"),
        save_t = l.t("Save", "Speichern"),
        devices_t = l.t("Signed-in devices", "Angemeldete Geräte"),
    );
    page(l, &s, Some(&v), "/account/admin/users", &name, &body)
}

#[derive(Deserialize)]
pub struct EditUserForm {
    quota: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    is_admin: Option<String>,
    #[serde(default)]
    disabled: Option<String>,
}

pub async fn admin_update(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<i64>, Form(f): Form<EditUserForm>) -> Response {
    let s = settings::load(&state).await;
    let Some(v) = guard(&state, &headers, &s).await.filter(|v| v.is_admin) else { return forbidden(lang(&headers)) };
    let back = |m: &str| Redirect::to(&format!("/account/admin/users/{id}?m={m}")).into_response();
    let Some(gb) = parse_gb(&f.quota) else { return back("quota") };
    if !f.password.is_empty() && f.password.chars().count() < s.min_password_len as usize {
        return back("pw_short");
    }
    let me = id == v.id;
    // Das eigene Konto bleibt Admin und aktiv (sonst sperrt man sich aus).
    let (is_admin, disabled) = if me { (true, false) } else { (f.is_admin.is_some(), f.disabled.is_some()) };
    let r = sqlx::query("UPDATE users SET quota_bytes = ?, is_admin = ?, disabled = ? WHERE id = ?")
        .bind((gb * GB) as i64)
        .bind(is_admin)
        .bind(disabled)
        .bind(id)
        .execute(&state.db)
        .await;
    if r.map(|r| r.rows_affected()).unwrap_or(0) == 0 {
        return Redirect::to("/account/admin").into_response();
    }
    if disabled {
        let _ = sqlx::query("DELETE FROM sessions WHERE user_id = ?").bind(id).execute(&state.db).await;
    }
    if !f.password.is_empty() && auth::set_password(&state, id, &f.password, if me { Some(&v.token_hash) } else { None }).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    back("saved")
}

pub async fn admin_signout(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<i64>) -> Response {
    let s = settings::load(&state).await;
    let Some(v) = guard(&state, &headers, &s).await.filter(|v| v.is_admin) else { return forbidden(lang(&headers)) };
    let _ = sqlx::query("DELETE FROM sessions WHERE user_id = ? AND token_hash != ?").bind(id).bind(&v.token_hash).execute(&state.db).await;
    Redirect::to(&format!("/account/admin/users/{id}?m=signedout")).into_response()
}

#[derive(Deserialize)]
pub struct DeleteForm {
    confirm: String,
}

pub async fn admin_delete(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<i64>, Form(f): Form<DeleteForm>) -> Response {
    let s = settings::load(&state).await;
    let Some(v) = guard(&state, &headers, &s).await.filter(|v| v.is_admin) else { return forbidden(lang(&headers)) };
    if id == v.id {
        return Redirect::to(&format!("/account/admin/users/{id}?m=self")).into_response();
    }
    let name: Option<(String,)> = sqlx::query_as("SELECT username FROM users WHERE id = ?").bind(id).fetch_optional(&state.db).await.unwrap_or(None);
    let Some((name,)) = name else { return Redirect::to("/account/admin").into_response() };
    if !f.confirm.trim().eq_ignore_ascii_case(&name) {
        return Redirect::to(&format!("/account/admin/users/{id}?m=confirm")).into_response();
    }
    if crate::admin::remove_user(&state, id).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    Redirect::to("/account/admin?m=deleted").into_response()
}

// ---------------------------------------------------------------- Einstellungen

pub async fn admin_settings(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<Msg>) -> Response {
    let l = lang(&headers);
    let s = settings::load(&state).await;
    let Some(v) = admin_viewer(&state, &headers, &s).await else { return Redirect::to("/account").into_response() };
    let checked = |b: bool| if b { " checked" } else { "" };
    let body = format!(
        r#"{flash}
<p class="crumbs"><a href="/account/admin">{admin_t}</a><span class="sep">›</span>{title}</p>
<h1>{title}</h1>
<form method="post" action="/account/admin/settings" class="form">
<h2>{general_t}</h2>
<div class="card pad form">
<label>{site_t}<input class="input" name="site_name" value="{site}" maxlength="40" required></label>
</div>

<h2>{accounts_t}</h2>
<div class="card pad form">
<label class="check"><input type="checkbox" name="open_registration" value="1"{reg}> {reg_t}</label>
<p class="hint">{reg_hint}</p>
<div class="grid2">
<label>{quota_t}<input class="input" name="default_quota_gb" value="{quota}" inputmode="decimal" required></label>
<label>{minpw_t}<input class="input" name="min_password_len" value="{minpw}" inputmode="numeric" required></label>
</div>
<label>{websess_t}<input class="input" name="web_session_days" value="{websess}" inputmode="numeric" required></label>
</div>

<h2>{links_t}</h2>
<div class="card pad form">
<label class="check"><input type="checkbox" name="links_enabled" value="1"{links}> {links_c}</label>
<p class="hint">{links_hint}</p>
<label class="check"><input type="checkbox" name="guest_upload" value="1"{guest}> {guest_c}</label>
<p class="hint">{guest_hint}</p>
<label>{expiry_t}<input class="input" name="link_expiry_days" value="{expiry}" inputmode="numeric" required></label>
<p class="hint">{expiry_hint}</p>
</div>
<div class="save-row"><button class="btn primary" type="submit">{save_t}</button></div>
</form>"#,
        flash = flash(l, &q.m),
        admin_t = l.t("Administration", "Verwaltung"),
        title = l.t("Server settings", "Server-Einstellungen"),
        general_t = l.t("General", "Allgemein"),
        site_t = l.t("Name shown on the start page and this website", "Name auf der Startseite und dieser Website"),
        site = esc(&s.site_name),
        accounts_t = l.t("Accounts", "Konten"),
        reg = checked(s.open_registration),
        reg_t = l.t("Open registration", "Freie Registrierung"),
        reg_hint = l.t("Anyone who knows the server address can create an account in the apps. Usually off.", "Jeder, der die Serveradresse kennt, kann sich in den Apps ein Konto anlegen. Meist aus."),
        quota_t = l.t("Storage for new accounts (GB)", "Speicher für neue Konten (GB)"),
        quota = gb_text(l, s.default_quota_bytes()),
        minpw_t = l.t("Minimum password length", "Mindestlänge für Passwörter"),
        minpw = s.min_password_len,
        websess_t = l.t("Sign out of this website after inactivity (days)", "Abmeldung von dieser Website nach Inaktivität (Tage)"),
        websess = s.web_session_days,
        links_t = l.t("Share links", "Freigabe-Links"),
        links = checked(s.links_enabled),
        links_c = l.t("Allow share links", "Freigabe-Links erlauben"),
        links_hint = l.t("When off, no new links can be created and existing links stop working until you turn this back on.", "Aus: Es lassen sich keine neuen Links anlegen, und bestehende Links funktionieren nicht, bis du das wieder einschaltest."),
        guest = checked(s.guest_upload),
        guest_c = l.t("Allow guest uploads", "Uploads von Gästen erlauben"),
        guest_hint = l.t("Guests can only upload into folders whose link allows it.", "Gäste können nur in Ordner hochladen, deren Link das erlaubt."),
        expiry_t = l.t("New links expire after (days, 0 = never)", "Neue Links laufen ab nach (Tage, 0 = nie)"),
        expiry = s.link_expiry_days,
        expiry_hint = l.t("Applies to links created from now on. Each link's expiry can still be changed in the apps.", "Gilt für Links, die ab jetzt erstellt werden. Das Ablaufdatum lässt sich pro Link in den Apps ändern."),
        save_t = l.t("Save settings", "Einstellungen speichern"),
    );
    page(l, &s, Some(&v), "/account/admin/settings", l.t("Server settings", "Server-Einstellungen"), &body)
}

#[derive(Deserialize)]
pub struct SettingsForm {
    site_name: String,
    #[serde(default)]
    open_registration: Option<String>,
    default_quota_gb: String,
    min_password_len: String,
    web_session_days: String,
    #[serde(default)]
    links_enabled: Option<String>,
    #[serde(default)]
    guest_upload: Option<String>,
    link_expiry_days: String,
}

pub async fn admin_save_settings(State(state): State<AppState>, headers: HeaderMap, Form(f): Form<SettingsForm>) -> Response {
    let s = settings::load(&state).await;
    if guard(&state, &headers, &s).await.filter(|v| v.is_admin).is_none() {
        return forbidden(lang(&headers));
    }
    let num = |v: &str| v.trim().parse::<u32>().ok();
    let (Some(quota), Some(minpw), Some(websess), Some(expiry)) = (parse_gb(&f.default_quota_gb), num(&f.min_password_len), num(&f.web_session_days), num(&f.link_expiry_days)) else {
        return Redirect::to("/account/admin/settings?m=number").into_response();
    };
    let new = Settings {
        site_name: f.site_name.trim().chars().take(40).collect::<String>(),
        open_registration: f.open_registration.is_some(),
        default_quota_gb: quota,
        links_enabled: f.links_enabled.is_some(),
        guest_upload: f.guest_upload.is_some(),
        link_expiry_days: expiry.min(3650),
        min_password_len: minpw.clamp(6, 128),
        web_session_days: websess.clamp(1, 365),
    };
    let new = if new.site_name.is_empty() { Settings { site_name: "DropCraft".into(), ..new } } else { new };
    if settings::save(&state, &new).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    Redirect::to("/account/admin/settings?m=saved").into_response()
}

const ACCOUNT_CSS: &str = r#"
.head-right{display:flex;align-items:center;gap:8px;flex-wrap:wrap;justify-content:flex-end}
.tabs{display:flex;gap:2px}
.tab{padding:6px 10px;border-radius:4px;color:#95a2b0;text-decoration:none;font-size:13px;font-weight:600}
.tab:hover{color:#e6ecf2;background:#1a2028}.tab.on{color:#19e3f0;background:#0f1d22}
.inline{display:inline;margin:0}
.btn.ghost{background:none;border-color:transparent;color:#95a2b0}.btn.ghost:hover{color:#e6ecf2;background:#1a2028}
.btn.small{height:30px;font-size:11px;padding:0 10px}
.btn.danger{background:#ef4455;border-color:#ef4455;color:#0b0e12}.btn.danger:hover{background:#ff6b78}
button.btn{cursor:pointer;font-family:inherit}
h2{font-size:13px;font-weight:600;letter-spacing:.06em;text-transform:uppercase;color:#95a2b0;margin:28px 0 10px}
.title-row{display:flex;align-items:center;gap:12px;flex-wrap:wrap;margin-bottom:16px}.title-row h1{margin:0;flex:1 1 auto}
.card{border:1px solid #28303a;border-radius:8px;background:#12161c}
.pad{padding:16px}
.label{font-size:12px;color:#95a2b0;text-transform:uppercase;letter-spacing:.06em;margin-bottom:4px}
.big-num{font-size:22px;margin-bottom:8px}.big-num .meta{font-size:13px;white-space:normal}
.meter{height:6px;border-radius:3px;background:#1a2028;overflow:hidden;margin-top:6px}.meter-fill{height:100%;background:#19e3f0}.meter-fill.warn{background:#fbbf24}
.list2{list-style:none;margin:0 0 10px;padding:0;overflow:hidden}
.list2>li+li{border-top:1px solid #1a2028}
.item{display:flex;align-items:center;gap:12px;padding:10px 14px;color:#e6ecf2;text-decoration:none}
a.item:hover{background:#1a2028}
.item .txt{gap:3px}.item .name{white-space:normal;overflow-wrap:anywhere}
.item .meta{white-space:normal}
.link{color:#e6ecf2;text-decoration:none}.link:hover{color:#19e3f0}
.chev{color:#55606c;font-size:20px}
.chip{display:inline-block;font-size:11px;font-weight:600;padding:1px 7px;border-radius:10px;background:#1a2028;color:#95a2b0;margin-left:4px;vertical-align:middle}
.chip.on{background:#0f1d22;color:#19e3f0}.chip.bad{background:#2a1418;color:#ef4455}
.form{display:flex;flex-direction:column;gap:12px}
.form label{display:flex;flex-direction:column;gap:6px;font-size:13px;color:#95a2b0}
.form label.check{flex-direction:row;align-items:center;gap:8px;color:#e6ecf2;font-size:14px}
.check input{width:16px;height:16px;accent-color:#19e3f0}
.input{height:40px;padding:0 12px;border:1px solid #28303a;border-radius:4px;background:#0b0e12;color:#e6ecf2;font:inherit;font-size:15px}
.input:focus{outline:none;border-color:#19e3f0}
.grid2{display:grid;grid-template-columns:1fr 1fr;gap:12px}
.narrow{max-width:380px;margin:0 auto}
.hint{color:#95a2b0;font-size:13px;margin:-4px 0 10px}.hint.center{text-align:center;margin-top:16px}
.card .hint{margin:-6px 0 0}
.flash{padding:10px 14px;border-radius:6px;margin-bottom:16px;font-size:14px}
.flash.ok{background:#0f1d22;border:1px solid #0c5c64;color:#19e3f0}.flash.bad{background:#2a1418;border:1px solid #5c1c24;color:#ff8a95}
.stats{display:grid;grid-template-columns:repeat(3,1fr);gap:10px}
.stat{border:1px solid #28303a;border-radius:8px;background:#12161c;padding:12px 14px}.stat .big-num{margin:4px 0 0;font-size:20px}
.danger-zone{border-color:#5c1c24}
.save-row{margin-top:16px}
.hero.small{padding:32px 0 16px}.hero.small h1{font-size:28px}
.empty{margin:0 0 10px}
@media (max-width:560px){.grid2{grid-template-columns:1fr}.stats{grid-template-columns:1fr 1fr}header{flex-wrap:wrap}.head-right{width:100%;justify-content:space-between}}
"#;
