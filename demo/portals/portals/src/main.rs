//! Mock supplier portals for the Panoptes demo: 20 fictional suppliers on one
//! port, each with its own login, theme, invoice list, filters, and PDF
//! downloads. There is deliberately no API. A staged challenge page, clearly
//! marked as a demo, can be switched on per portal to exercise the human
//! handoff.
//!
//! usage: portals DATA_DIR    (reads suppliers.json, truth.jsonl, pdf/)
//! env:   PORTALS_ADDR (default 127.0.0.1:8090), CAPTCHA_PORTALS=slug,slug

use axum::{
    extract::{Form, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
    Router,
};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    io::Read as _,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Deserialize, Clone)]
struct Theme {
    bg: String,
    fg: String,
    accent: String,
    font: String,
    list: String,
    page_size: usize,
    filter: String,
    list_label: String,
    user_field: String,
}

#[derive(Deserialize, Clone)]
struct Login {
    user: String,
    password: String,
}

#[derive(Deserialize, Clone)]
struct Supplier {
    id: u32,
    name: String,
    slug: String,
    portal: Theme,
    login: Login,
}

#[derive(Deserialize, Clone)]
struct Display {
    date: String,
    total: String,
    due: String,
}

#[derive(Deserialize, Clone)]
struct Invoice {
    invoice_no: String,
    file: String,
    slug: String,
    date: String,
    status: String,
    display: Display,
}

struct Session {
    slug: String,
    passed: bool,
    code: String,
}

struct App {
    suppliers: HashMap<String, Supplier>,
    order: Vec<String>,
    invoices: HashMap<String, Vec<Invoice>>,
    pdf_dir: PathBuf,
    sessions: Mutex<HashMap<String, Session>>,
    captcha: Mutex<HashSet<String>>,
}

type St = State<Arc<App>>;

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .expect("read /dev/urandom");
    b.iter().fold(String::new(), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn page(sp: &Supplier, title: &str, body: &str) -> Html<String> {
    let t = &sp.portal;
    // Layout varies with the supplier: nav on top, on the left, or in a footer.
    let nav = format!(
        r#"<nav><a href="/{s}/invoices">{l}</a> <a href="/{s}/logout">Sign out</a></nav>"#,
        s = sp.slug,
        l = esc(&t.list_label)
    );
    let (pre, post) = match sp.id % 3 {
        0 => (nav.clone(), String::new()),
        1 => (String::new(), nav.clone()),
        _ => (format!(r#"<aside>{nav}</aside>"#), String::new()),
    };
    Html(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>{title} | {name}</title>
<style>
body{{background:{bg};color:{fg};font-family:{font};margin:0}}
header{{background:{accent};color:#fff;padding:14px 24px;font-size:20px}}
main{{padding:20px 24px;max-width:1100px}}
a{{color:{accent}}} header a{{color:#fff}}
aside{{float:left;width:160px;padding:20px}} aside a{{display:block;margin:6px 0}}
nav{{padding:10px 24px;border-bottom:1px solid {accent}}} nav a{{margin-right:16px}}
table{{border-collapse:collapse;width:100%}} td,th{{padding:7px 10px;border-bottom:1px solid #8884;text-align:left}}
.card{{display:inline-block;width:240px;margin:8px;padding:12px;border:1px solid {accent};border-radius:6px;vertical-align:top}}
.row{{padding:10px 0;border-bottom:1px dashed #8886}}
.demo{{background:#ffe08a;color:#000;padding:8px 12px;border:2px dashed #b58900}}
input,select,button{{font:inherit;padding:5px 8px;margin:4px}}
</style></head><body>
<header>{name} <small style="opacity:.8">Supplier portal</small></header>
{pre}<main><h2>{title}</h2>{body}</main>{post}
</body></html>"#,
        title = esc(title),
        name = esc(&sp.name),
        bg = t.bg,
        fg = t.fg,
        font = t.font,
        accent = t.accent,
    ))
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

/// The session for this portal, if signed in. `None` means sign in first.
fn session(app: &App, headers: &HeaderMap, slug: &str) -> Option<(String, bool)> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    let name = format!("psid_{slug}=");
    let sid = cookie
        .split(';')
        .map(str::trim)
        .find_map(|c| c.strip_prefix(&name))?;
    let s = app.sessions.lock().unwrap();
    let sess = s.get(sid)?;
    (sess.slug == slug).then(|| (sid.to_string(), sess.passed))
}

/// Gate for pages behind login and, when switched on, the demo challenge.
// The error is the redirect handlers return as-is; boxing it buys nothing.
#[allow(clippy::result_large_err)]
fn gate(app: &App, headers: &HeaderMap, slug: &str) -> Result<(), Response> {
    match session(app, headers, slug) {
        None => Err(Redirect::to(&format!("/{slug}/login")).into_response()),
        Some((_, passed)) if !passed && app.captcha.lock().unwrap().contains(slug) => {
            Err(Redirect::to(&format!("/{slug}/challenge")).into_response())
        }
        Some(_) => Ok(()),
    }
}

async fn index(State(app): St) -> Html<String> {
    let mut b = String::from(
        "<!doctype html><title>Demo supplier portals</title><h1>Demo supplier portals</h1>\
         <p>Fictional suppliers for the Panoptes demo.</p><ul>",
    );
    for slug in &app.order {
        let sp = &app.suppliers[slug];
        let _ = write!(
            b,
            r#"<li><a href="/{s}/login">{n}</a></li>"#,
            s = sp.slug,
            n = esc(&sp.name)
        );
    }
    b.push_str("</ul>");
    Html(b)
}

async fn root(Path(slug): Path<String>) -> Redirect {
    Redirect::to(&format!("/{slug}/login"))
}

async fn login_form(State(app): St, Path(slug): Path<String>) -> Response {
    let Some(sp) = app.suppliers.get(&slug) else {
        return not_found();
    };
    let (label, kind) = match sp.portal.user_field.as_str() {
        "email" => ("Email or user ID", "text"),
        "account" => ("Account number", "text"),
        _ => ("Username", "text"),
    };
    let body = format!(
        r#"<form method="post" action="/{s}/login">
<label>{label}<br><input name="{f}" type="{kind}" autocomplete="username"></label><br>
<label>Password<br><input name="password" type="password" autocomplete="current-password"></label><br>
<button type="submit">{btn}</button></form>"#,
        s = sp.slug,
        f = sp.portal.user_field,
        btn = ["Sign in", "Log in", "Continue", "Enter portal"][sp.id as usize % 4],
    );
    page(sp, "Sign in", &body).into_response()
}

async fn login(
    State(app): St,
    Path(slug): Path<String>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let Some(sp) = app.suppliers.get(&slug) else {
        return not_found();
    };
    let user = form
        .get(&sp.portal.user_field)
        .map(String::as_str)
        .unwrap_or("");
    let pass = form.get("password").map(String::as_str).unwrap_or("");
    if user != sp.login.user || pass != sp.login.password {
        let body = format!(
            r#"<p style="color:#c00">Sign-in failed. Check your credentials.</p><p><a href="/{}/login">Try again</a></p>"#,
            sp.slug
        );
        return (StatusCode::UNAUTHORIZED, page(sp, "Sign in", &body)).into_response();
    }
    let sid = random_hex(16);
    app.sessions.lock().unwrap().insert(
        sid.clone(),
        Session {
            slug: slug.clone(),
            passed: false,
            code: String::new(),
        },
    );
    let next = if app.captcha.lock().unwrap().contains(&slug) {
        "challenge"
    } else {
        "invoices"
    };
    (
        [(
            header::SET_COOKIE,
            format!("psid_{slug}={sid}; Path=/{slug}; HttpOnly; SameSite=Lax"),
        )],
        Redirect::to(&format!("/{slug}/{next}")),
    )
        .into_response()
}

async fn logout(State(app): St, Path(slug): Path<String>, headers: HeaderMap) -> Response {
    if let Some((sid, _)) = session(&app, &headers, &slug) {
        app.sessions.lock().unwrap().remove(&sid);
    }
    Redirect::to(&format!("/{slug}/login")).into_response()
}

/// The staged challenge: a distorted code drawn as SVG. It is a demo of the
/// human handoff, not a real CAPTCHA, and says so on the page.
fn challenge_svg(code: &str, seed: &str) -> String {
    let r: Vec<u8> = seed.bytes().collect();
    let mut svg = String::from(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="260" height="80" style="background:#eee">"##,
    );
    for i in 0..6 {
        let a = r[i % r.len()] as u32;
        let _ = write!(
            svg,
            r##"<line x1="0" y1="{}" x2="260" y2="{}" stroke="#999" stroke-width="2"/>"##,
            a % 80,
            (a * 7) % 80
        );
    }
    for (i, ch) in code.chars().enumerate() {
        let a = r[(i * 3) % r.len()] as i32;
        let _ = write!(
            svg,
            r##"<text x="{}" y="{}" font-size="38" font-family="monospace" fill="#333" transform="rotate({} {} 40)">{}</text>"##,
            20 + i * 46,
            50 + (a % 14) - 7,
            (a % 40) - 20,
            30 + i * 46,
            ch
        );
    }
    svg.push_str("</svg>");
    svg
}

async fn challenge_form(State(app): St, Path(slug): Path<String>, headers: HeaderMap) -> Response {
    let Some(sp) = app.suppliers.get(&slug) else {
        return not_found();
    };
    let Some((sid, _)) = session(&app, &headers, &slug) else {
        return Redirect::to(&format!("/{slug}/login")).into_response();
    };
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let raw = random_hex(10);
    let code: String = raw
        .as_bytes()
        .chunks(2)
        .take(5) // five characters fit the 260 px image
        .map(|c| {
            ALPHABET[u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap() as usize
                % ALPHABET.len()] as char
        })
        .collect();
    if let Some(s) = app.sessions.lock().unwrap().get_mut(&sid) {
        s.code = code.clone();
    }
    let body = format!(
        r#"<p class="demo">DEMO CHALLENGE: staged for the Panoptes human-handoff demo. This is not a real CAPTCHA.</p>
<p>Please confirm you are a person. Type the characters shown.</p>
<div id="challenge">{svg}</div>
<form method="post" action="/{s}/challenge"><input name="answer" autocomplete="off"><button type="submit">Verify</button></form>"#,
        svg = challenge_svg(&code, &raw),
        s = sp.slug
    );
    page(sp, "Verification required", &body).into_response()
}

async fn challenge(
    State(app): St,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let Some((sid, _)) = session(&app, &headers, &slug) else {
        return Redirect::to(&format!("/{slug}/login")).into_response();
    };
    let answer = form
        .get("answer")
        .map(|a| a.trim().to_uppercase())
        .unwrap_or_default();
    let mut s = app.sessions.lock().unwrap();
    let sess = s.get_mut(&sid).unwrap();
    let ok = !sess.code.is_empty() && answer == sess.code;
    sess.passed |= ok;
    Redirect::to(&format!(
        "/{slug}/{}",
        if ok { "invoices" } else { "challenge" }
    ))
    .into_response()
}

#[derive(Deserialize, Default)]
struct ListQuery {
    page: Option<usize>,
    q: Option<String>,
    from: Option<String>,
    to: Option<String>,
    status: Option<String>,
    month: Option<String>,
}

async fn list(
    State(app): St,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Response {
    let Some(sp) = app.suppliers.get(&slug) else {
        return not_found();
    };
    if let Err(r) = gate(&app, &headers, &slug) {
        return r;
    }
    let t = &sp.portal;
    let nonempty = |o: &Option<String>| o.as_deref().filter(|s| !s.is_empty()).map(str::to_string);
    let (qs, from, to, status, month) = (
        nonempty(&q.q),
        nonempty(&q.from),
        nonempty(&q.to),
        nonempty(&q.status),
        nonempty(&q.month),
    );
    let all = app.invoices.get(&slug).map(Vec::as_slice).unwrap_or(&[]);
    let rows: Vec<&Invoice> = all
        .iter()
        .filter(|i| {
            qs.as_deref()
                .is_none_or(|s| i.invoice_no.to_lowercase().contains(&s.to_lowercase()))
        })
        .filter(|i| from.as_deref().is_none_or(|f| i.date.as_str() >= f))
        .filter(|i| to.as_deref().is_none_or(|f| i.date.as_str() <= f))
        .filter(|i| {
            status
                .as_deref()
                .is_none_or(|s| s == "all" || i.status.eq_ignore_ascii_case(s))
        })
        .filter(|i| month.as_deref().is_none_or(|m| i.date.starts_with(m)))
        .collect();
    let per = t.page_size.max(1);
    let pages = rows.len().div_ceil(per).max(1);
    let pg = q.page.unwrap_or(1).clamp(1, pages);
    let shown = &rows[((pg - 1) * per).min(rows.len())..(pg * per).min(rows.len())];

    let mut b = String::new();
    // Each portal filters differently.
    let keep = format!(
        "{}{}{}{}{}",
        qs.as_deref()
            .map(|v| format!("&q={}", esc(v)))
            .unwrap_or_default(),
        from.as_deref()
            .map(|v| format!("&from={v}"))
            .unwrap_or_default(),
        to.as_deref()
            .map(|v| format!("&to={v}"))
            .unwrap_or_default(),
        status
            .as_deref()
            .map(|v| format!("&status={v}"))
            .unwrap_or_default(),
        month
            .as_deref()
            .map(|v| format!("&month={v}"))
            .unwrap_or_default(),
    );
    let _ = write!(b, r#"<form method="get" action="/{}/invoices">"#, sp.slug);
    match t.filter.as_str() {
        "daterange" => b.push_str(
            r#"From <input type="date" name="from"> to <input type="date" name="to"> <button>Apply</button>"#,
        ),
        "status" => b.push_str(
            r#"<select name="status"><option value="all">All statuses</option><option>Open</option><option>Paid</option></select> <button>Filter</button>"#,
        ),
        "month" => b.push_str(
            r#"<select name="month"><option value="">Any month</option><option value="2026-08">August 2026</option><option value="2026-09">September 2026</option><option value="2026-10">October 2026</option><option value="2026-11">November 2026</option></select> <button>Show</button>"#,
        ),
        _ => b.push_str(r#"<input name="q" placeholder="Invoice number"> <button>Search</button>"#),
    }
    b.push_str("</form>");
    let _ = write!(b, "<p>{} documents</p>", rows.len());

    let dl = |i: &Invoice| {
        // Card portals make you open the document first; the rest link the PDF.
        if t.list == "cards" {
            format!(r#"<a href="/{}/invoices/{}">View</a>"#, sp.slug, i.file)
        } else {
            format!(
                r#"<a href="/{}/download/{}.pdf">Download PDF</a>"#,
                sp.slug, i.file
            )
        }
    };
    match t.list.as_str() {
        "cards" => {
            for i in shown {
                let _ = write!(
                    b,
                    r#"<div class="card"><b>{}</b><br>{}<br>{}<br>Status: {}<br>{}</div>"#,
                    esc(&i.invoice_no),
                    esc(&i.display.date),
                    esc(&i.display.total),
                    i.status,
                    dl(i)
                );
            }
        }
        "list" => {
            for i in shown {
                let _ = write!(
                    b,
                    r#"<div class="row">{} | issued {} | due {} | {} | {} | {}</div>"#,
                    esc(&i.invoice_no),
                    esc(&i.display.date),
                    esc(&i.display.due),
                    esc(&i.display.total),
                    i.status,
                    dl(i)
                );
            }
        }
        _ => {
            b.push_str("<table><tr><th>Number</th><th>Date</th><th>Due</th><th>Amount</th><th>Status</th><th></th></tr>");
            for i in shown {
                let _ =
                    write!(
                    b,
                    "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    esc(&i.invoice_no), esc(&i.display.date), esc(&i.display.due),
                    esc(&i.display.total), i.status, dl(i)
                );
            }
            b.push_str("</table>");
        }
    }
    b.push_str("<p>");
    if pg > 1 {
        let _ = write!(
            b,
            r#"<a href="/{}/invoices?page={}{}">Previous</a> "#,
            sp.slug,
            pg - 1,
            keep
        );
    }
    let _ = write!(b, "Page {pg} of {pages}");
    if pg < pages {
        let _ = write!(
            b,
            r#" <a href="/{}/invoices?page={}{}">Next</a>"#,
            sp.slug,
            pg + 1,
            keep
        );
    }
    b.push_str("</p>");
    page(sp, &t.list_label, &b).into_response()
}

fn find<'a>(app: &'a App, slug: &str, file: &str) -> Option<&'a Invoice> {
    app.invoices.get(slug)?.iter().find(|i| i.file == file)
}

async fn detail(
    State(app): St,
    Path((slug, file)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(sp) = app.suppliers.get(&slug) else {
        return not_found();
    };
    if let Err(r) = gate(&app, &headers, &slug) {
        return r;
    }
    let Some(i) = find(&app, &slug, &file) else {
        return not_found();
    };
    let body = format!(
        r#"<table><tr><th>Number</th><td>{}</td></tr><tr><th>Issued</th><td>{}</td></tr><tr><th>Due</th><td>{}</td></tr>
<tr><th>Amount</th><td>{}</td></tr><tr><th>Status</th><td>{}</td></tr></table>
<p><a href="/{}/download/{}.pdf">Download PDF</a></p>"#,
        esc(&i.invoice_no),
        esc(&i.display.date),
        esc(&i.display.due),
        esc(&i.display.total),
        i.status,
        sp.slug,
        i.file
    );
    page(sp, &format!("Document {}", i.invoice_no), &body).into_response()
}

async fn download(
    State(app): St,
    Path((slug, file)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if !app.suppliers.contains_key(&slug) {
        return not_found();
    }
    if let Err(r) = gate(&app, &headers, &slug) {
        return r;
    }
    let Some(i) = file.strip_suffix(".pdf").and_then(|f| find(&app, &slug, f)) else {
        return not_found();
    };
    let Ok(bytes) = tokio::fs::read(app.pdf_dir.join(format!("{}.pdf", i.file))).await else {
        return not_found();
    };
    let name: String = i
        .invoice_no
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    (
        [
            (header::CONTENT_TYPE, "application/pdf".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!(r#"attachment; filename="{name}.pdf""#),
            ),
        ],
        bytes,
    )
        .into_response()
}

#[derive(Deserialize)]
struct CaptchaQuery {
    portal: Option<String>,
    on: Option<u8>,
}

/// GET /admin/captcha?portal=SLUG&on=1|0 switches the staged challenge.
async fn admin_captcha(State(app): St, Query(q): Query<CaptchaQuery>) -> String {
    let mut c = app.captcha.lock().unwrap();
    if let (Some(p), Some(on)) = (q.portal, q.on) {
        if app.suppliers.contains_key(&p) {
            if on == 1 {
                c.insert(p);
            } else {
                c.remove(&p);
            }
        }
    }
    let mut v: Vec<&String> = c.iter().collect();
    v.sort();
    format!(
        "challenge on: {}\n",
        v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",")
    )
}

#[tokio::main]
async fn main() {
    // Frozen and parked kiosks keep their keep-alive connections open, so
    // 700 agents hold over a thousand sockets; the default soft limit of
    // 1024 stalled every request at that scale.
    use rustix::process::{getrlimit, setrlimit, Resource};
    let mut nofile = getrlimit(Resource::Nofile);
    nofile.current = nofile.maximum;
    if let Err(e) = setrlimit(Resource::Nofile, nofile) {
        eprintln!("portals: cannot raise the open-file limit: {e}");
    }
    let dir = PathBuf::from(std::env::args().nth(1).expect("usage: portals DATA_DIR"));
    let suppliers: Vec<Supplier> =
        serde_json::from_slice(&std::fs::read(dir.join("suppliers.json")).expect("suppliers.json"))
            .expect("parse suppliers.json");
    let mut invoices: HashMap<String, Vec<Invoice>> = HashMap::new();
    for line in std::fs::read_to_string(dir.join("truth.jsonl"))
        .expect("truth.jsonl")
        .lines()
    {
        if line.trim().is_empty() {
            continue;
        }
        let i: Invoice = serde_json::from_str(line).expect("parse truth.jsonl");
        invoices.entry(i.slug.clone()).or_default().push(i);
    }
    for v in invoices.values_mut() {
        v.sort_by(|a, b| b.date.cmp(&a.date).then(b.invoice_no.cmp(&a.invoice_no)));
    }
    let captcha: HashSet<String> = std::env::var("CAPTCHA_PORTALS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let app = Arc::new(App {
        order: suppliers.iter().map(|s| s.slug.clone()).collect(),
        suppliers: suppliers.into_iter().map(|s| (s.slug.clone(), s)).collect(),
        invoices,
        pdf_dir: dir.join("pdf"),
        sessions: Mutex::new(HashMap::new()),
        captcha: Mutex::new(captcha),
    });
    let router = Router::new()
        .route("/", get(index))
        .route("/admin/captcha", get(admin_captcha))
        .route("/{slug}", get(root))
        .route("/{slug}/login", get(login_form).post(login))
        .route("/{slug}/logout", get(logout))
        .route("/{slug}/challenge", get(challenge_form).post(challenge))
        .route("/{slug}/invoices", get(list))
        .route("/{slug}/invoices/{file}", get(detail))
        .route("/{slug}/download/{file}", get(download))
        .with_state(app);
    let addr = std::env::var("PORTALS_ADDR").unwrap_or_else(|_| "127.0.0.1:8090".into());
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    eprintln!("portals listening on http://{addr}");
    axum::serve(listener, router).await.expect("serve");
}
