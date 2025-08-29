// ==== tetap: use statements kamu di atas ====
use serde::{Deserialize, Serialize};

// … kode lama …

#[event(fetch)]
async fn main(req: Request, env: Env, _: Context) -> Result<Response> {
    let uuid = env
        .var("UUID")
        .map(|x| Uuid::parse_str(&x.to_string()).unwrap_or_default())?;
    let host = req.url()?.host().map(|x| x.to_string()).unwrap_or_default();
    let main_page_url = env.var("MAIN_PAGE_URL").map(|x| x.to_string()).unwrap();
    let sub_page_url = env.var("SUB_PAGE_URL").map(|x| x.to_string()).unwrap();

    let config = Config {
        uuid,
        host: host.clone(),
        proxy_addr: host,
        proxy_port: 443,
        main_page_url,
        sub_page_url,
    };

    Router::with_data(config)
        .on_async("/", fe)
        .on_async("/sub", sub)
        .on("/link", link)

        // ⬇⬇⬇ TAMBAHKAN: endpoint Clash provider YAML
        .on_async("/api/vless", api_vless)

        // ⬇️ wildcard harus PALING AKHIR agar tidak “menangkap” /api/vless
        .on_async("/:proxyip", tunnel)
        .run(req, env)
        .await
}

// ====== util fetch html tetap ======

async fn get_response_from_url(url: String) -> Result<Response> {
    let req = Fetch::Url(Url::parse(url.as_str())?);
    let mut res = req.send().await?;
    Response::from_html(res.text().await?)
}

// ====== FE & SUB tetap ======

async fn fe(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(cx.data.main_page_url).await
}

async fn sub(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(cx.data.sub_page_url).await
}

// ====== NEW: /api/vless (Clash provider YAML) ======

#[derive(Serialize)]
struct YamlProxy<'a> {
    #[serde(rename = "name")]
    name: String,
    #[serde(rename = "network")]
    network: &'a str, // "ws"
    #[serde(rename = "port")]
    port: String,
    #[serde(rename = "server")]
    server: String, // subdomain (bug host)
    #[serde(rename = "servername")]
    servername: String, // subdomain.domain
    #[serde(rename = "tls")]
    tls: bool,
    #[serde(rename = "type")]
    typ: &'a str, // "vless"
    #[serde(rename = "packet-encoding")]
    packet_encoding: &'a str, // "packetaddr"
    #[serde(rename = "uuid")]
    uuid: String,
    #[serde(rename = "ws-opts")]
    ws_opts: WsOpts,
}

#[derive(Serialize)]
struct WsOpts {
    headers: HashMap<String, String>,
    path: String,
}

#[derive(Serialize)]
struct ProviderOut<'a> {
    proxies: Vec<YamlProxy<'a>>,
}

// helper: ambil query param dengan default
fn q<'a>(p: &'a web_sys::UrlSearchParams, k: &str, d: &'a str) -> String

}

async fn api_vless(req: Request, cx: RouteContext<Config>) -> Result<Response> {
    let url = req.url()?;
    let params = url.search_params();

    // --- params ---
    let cc = q(&params, "cc", "jp").to_uppercase(); // country code, default JP
    let tls = q(&params, "tls", "true") == "true";
    let _cdn = q(&params, "cdn", "true") == "true"; // disimpan kalau mau dipakai di nama
    let limit: usize = q(&params, "limit", "10").parse().unwrap_or(10);
    let format = q(&params, "format", "clash-provider");
    let domain = q(&params, "domain", &cx.data.host); // domain CDN/bug SNI
    let subdomain = q(&params, "subdomain", "ava.game.naver.com");
    let bug = q(&params, "bug", &subdomain); // untuk Host header

    if format != "clash-provider" {
        return Response::error("unsupported format", 400);
    }

    // --- ambil daftar IP:PORT dari KV "proxy_kv" seperti di tunnel() ---
    let kv = cx.kv("SIREN")?;
    let mut proxy_kv_str = kv
        .get("proxy_kv")
        .text()
        .await?
        .unwrap_or_else(|| "".to_string());

    if proxy_kv_str.is_empty() {
        // sama persis dengan logika di tunnel(): cache 24 jam
        let req = Fetch::Url(Url::parse(
            "https://raw.githubusercontent.com/FoolVPN-ID/Nautica/refs/heads/main/kvProxyList.json",
        )?);
        let mut res = req.send().await?;
        if res.status_code() == 200 {
            proxy_kv_str = res.text().await?.to_string();
            kv.put("proxy_kv", &proxy_kv_str)?
                .expiration_ttl(60 * 60 * 24)
                .execute()
                .await?;
        } else {
            return Response::error("cannot load proxy_kv", 502);
        }
    }

    let proxy_kv: HashMap<String, Vec<String>> = serde_json::from_str(&proxy_kv_str)?;
    let ips = proxy_kv.get(&cc).cloned().unwrap_or_default();
    if ips.is_empty() {
        return Response::error("no proxies for cc", 404);
    }

    // --- susun YAML provider ---
    // nama tampil: "1 🇯🇵 JP ... TLS"
    let flag = match cc.as_str() {
        "JP" => "🇯🇵",
        "KR" => "🇰🇷",
        "US" => "🇺🇸",
        "SG" => "🇸🇬",
        _ => "🏳️",
    };

    let server = subdomain.clone(); // host yg dipakai klien
    let servername = format!("{}.{}", subdomain, domain); // SNI/Host
    let port_str = "443"; // dari contohmu

    let mut headers = HashMap::new();
    headers.insert("Host".to_string(), servername.clone());
    // UA random (opsional)—biar sederhana, gunakan satu saja:
    headers.insert(
        "User-Agent".to_string(),
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.0.0 Safari/537.36".to_string(),
    );

    let mut out: Vec<YamlProxy> = Vec::new();
    for (i, ipport) in ips.into_iter().take(limit).enumerate() {
        let (ip, port) = ipport
            .split_once(':')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap_or_else(|| (ipport.clone(), "443".to_string()));

        let name = format!("{} {} {} {} {}", i + 1, flag, cc, "CDN", if tls { "TLS" } else { "NTLS" });

        out.push(YamlProxy {
            name,
            network: "ws",
            port: port_str.to_string(),
            server: server.clone(),
            servername: servername.clone(),
            tls,
            typ: "vless",
            packet_encoding: "packetaddr",
            uuid: cx.data.uuid.to_string(),
            ws_opts: WsOpts {
                headers: headers.clone(),
                // path yang kamu minta: /free/<ip:port>
                path: format!("/free/{}:{}", ip, port),
            },
        });
    }

    // Serialize ke YAML
    let body = serde_yaml::to_string(&ProviderOut { proxies: out })
        .unwrap_or_else(|_| "proxies: []\n".to_string());

    let mut res = Response::ok(body)?;
    // provider biasanya dibaca sebagai text/yaml
    res.headers_mut()
        .set("content-type", "application/x-yaml; charset=utf-8")
        .ok();
    Ok(res)
}

// ====== tunnel() & link() kamu tetap, tidak diubah ======

// … sisanya tetap …
