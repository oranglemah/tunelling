use serde::Serialize; // ⬅️ tambahkan import ini

// … kode & imports kamu yang lain …

#[event(fetch)]
async fn main(req: Request, env: Env, _: Context) -> Result<Response> {
    let uuid = env
        .var("UUID")
        .map(|x| Uuid::parse_str(&x.to_string()).unwrap_or_default())?;
    let host = req.url()?.host().map(|x| x.to_string()).unwrap_or_default();
    let main_page_url = env.var("MAIN_PAGE_URL").map(|x| x.to_string()).unwrap();
    let sub_page_url = env.var("SUB_PAGE_URL").map(|x| x.to_string()).unwrap();
    let config = Config { uuid, host: host.clone(), proxy_addr: host, proxy_port: 443, main_page_url, sub_page_url };

    Router::with_data(config)
        .on_async("/", fe)
        .on_async("/sub", sub)
        .on("/link", link)
        // ⬇️ tambahkan ini, letakkan sebelum wildcard:
        .on_async("/api/vless", api_vless)
        // wildcard paling akhir:
        .on_async("/:proxyip", tunnel)
        .run(req, env)
        .await
}

// ===== util fetch HTML (tetap) =====
async fn get_response_from_url(url: String) -> Result<Response> {
    let req = Fetch::Url(Url::parse(url.as_str())?);
    let mut res = req.send().await?;
    Response::from_html(res.text().await?)
}

// ===== FE & SUB (tetap) =====
async fn fe(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(cx.data.main_page_url).await
}
async fn sub(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(cx.data.sub_page_url).await
}

// ===== YAML structs untuk Clash Provider =====
#[derive(Serialize)]
struct WsOpts {
    headers: std::collections::HashMap<String, String>,
    #[serde(rename = "path")]
    path: String,
}

#[derive(Serialize)]
struct YamlProxy<'a> {
    name: String,
    network: &'a str,                 // "ws"
    port: String,                     // "443"
    server: String,                   // subdomain (bug)
    servername: String,               // subdomain.domain
    tls: bool,
    #[serde(rename = "type")]
    typ: &'a str,                     // "vless"
    #[serde(rename = "packet-encoding")]
    packet_encoding: &'a str,         // "packetaddr"
    uuid: String,
    #[serde(rename = "ws-opts")]
    ws_opts: WsOpts,
}

#[derive(Serialize)]
struct ProviderOut<'a> {
    proxies: Vec<YamlProxy<'a>>,
}

// ===== handler: GET /api/vless =====
async fn api_vless(req: Request, cx: RouteContext<Config>) -> Result<Response> {
    let url = req.url()?;
    let params = url.search_params();

    // ambil param dgn default
    let cc = params.get("cc").unwrap_or("jp".into()).to_uppercase();
    let tls = params.get("tls").unwrap_or("true".into()) == "true";
    let _cdn = params.get("cdn").unwrap_or("true".into()) == "true";
    let limit: usize = params.get("limit").unwrap_or("10".into()).parse().unwrap_or(10);
    let format = params.get("format").unwrap_or("clash-provider".into());
    let domain = params.get("domain").unwrap_or(cx.data.host.clone());
    let subdomain = params.get("subdomain").unwrap_or("ava.game.naver.com".into());
    let bug = params.get("bug").unwrap_or(subdomain.clone());

    if format != "clash-provider" {
        return Response::error("unsupported format", 400);
    }

    // Ambil list IP:PORT dari KV, sama pola dgn tunnel()
    let kv = cx.kv("SIREN")?;
    let mut proxy_kv_str = kv.get("proxy_kv").text().await?.unwrap_or_default();
    if proxy_kv_str.is_empty() {
        let req = Fetch::Url(Url::parse("https://raw.githubusercontent.com/FoolVPN-ID/Nautica/refs/heads/main/kvProxyList.json")?);
        let mut res = req.send().await?;
        if res.status_code() == 200 {
            proxy_kv_str = res.text().await?.to_string();
            kv.put("proxy_kv", &proxy_kv_str)?.expiration_ttl(60 * 60 * 24).execute().await?;
        } else {
            return Response::error("cannot load proxy_kv", 502);
        }
    }
    let proxy_kv: HashMap<String, Vec<String>> = serde_json::from_str(&proxy_kv_str)?;
    let ips = proxy_kv.get(&cc).cloned().unwrap_or_default();
    if ips.is_empty() {
        return Response::error("no proxies for cc", 404);
    }

    // nama & flag
    let flag = match cc.as_str() { "JP" => "🇯🇵", "KR" => "🇰🇷", "US" => "🇺🇸", "SG" => "🇸🇬", _ => "🏳️" };
    let server = subdomain.clone();
    let servername = format!("{}.{}", subdomain, domain);

    // header default (boleh kamu acak/ubah)
    let mut headers = std::collections::HashMap::new();
    headers.insert("Host".to_string(), format!("{}.{}", bug, domain));
    headers.insert("User-Agent".to_string(), "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.0.0 Safari/537.36".to_string());

    // bentuk daftar proxies
    let mut out_list = Vec::new();
    for (i, ipport) in ips.into_iter().take(limit).enumerate() {
        let (ip, port) = ipport.split_once(':').map(|(a,b)|(a.to_string(), b.to_string())).unwrap_or((ipport.clone(), "443".to_string()));
        let name = format!("{} {} {} CDN {}", i+1, flag, cc, if tls { "TLS" } else { "NTLS" });

        out_list.push(YamlProxy {
            name,
            network: "ws",
            port: "443".into(),       // port ke CDN/bug (WSS)
            server: server.clone(),
            servername: format!("{}.{}", bug, domain),
            tls,
            typ: "vless",
            packet_encoding: "packetaddr",
            uuid: cx.data.uuid.to_string(),
            ws_opts: WsOpts {
                headers: headers.clone(),
                path: format!("/free/{}:{}", ip, port),  // path sesuai contohmu
            },
        });
    }

    // serialize YAML
    let body = serde_yaml::to_string(&ProviderOut { proxies: out_list })
        .unwrap_or_else(|_| "proxies: []\n".to_string());

    let mut res = Response::ok(body)?;
    res.headers_mut().set("content-type", "application/x-yaml; charset=utf-8").ok();
    Ok(res)
}
