use anyhow::{anyhow, Context};
use log::{error, info};
use parking_lot::{Mutex, RwLock};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tauri::http::header::HOST;
use tauri::http::{HeaderName, HeaderValue};
use tauri::Url;
use tokio::select;
use tokio_rustls::rustls::pki_types::DnsName;
use url::Host;
use wstunnel::protocols::dns::DnsResolver;
use wstunnel::protocols::tls;
use wstunnel::tunnel::client::{TlsClientConfig, WsClient, WsClientConfig};
use wstunnel::tunnel::connectors::{Socks5TunnelConnector, TcpTunnelConnector, UdpTunnelConnector};
use wstunnel::tunnel::listeners::{
    new_stdio_listener, HttpProxyTunnelListener, Socks5TunnelListener, TcpTunnelListener,
    UdpTunnelListener,
};
use wstunnel::tunnel::transport::{TransportAddr, TransportScheme};
use wstunnel::tunnel::{client, to_host_port, LocalProtocol, RemoteAddr};

const DEFAULT_CLIENT_UPGRADE_PATH_PREFIX: &str = "v1";

pub struct WsClientApi {}

impl WsClientApi {
    pub async fn connect(args: Box<Client>) -> anyhow::Result<()> {
        let (tls_certificate, tls_key) = if let (Some(cert), Some(key)) =
            (args.tls_certificate.as_ref(), args.tls_private_key.as_ref())
        {
            let tls_certificate = tls::load_certificates_from_pem(cert)
                .expect("Cannot load client TLS certificate (mTLS)");
            let tls_key = tls::load_private_key_from_file(key)
                .expect("Cannot load client TLS private key (mTLS)");
            (Some(tls_certificate), Some(tls_key))
        } else {
            (None, None)
        };

        let http_upgrade_path_prefix = if args
            .http_upgrade_path_prefix
            .eq(DEFAULT_CLIENT_UPGRADE_PATH_PREFIX)
        {
            // When using mTLS and no manual http upgrade path is specified configure the HTTP upgrade path
            // to be the common name (CN) of the client's certificate.
            tls_certificate
                .as_ref()
                .and_then(|certs| tls::find_leaf_certificate(certs.as_slice()))
                .and_then(|leaf_cert| tls::cn_from_certificate(&leaf_cert))
                .unwrap_or(args.http_upgrade_path_prefix)
        } else {
            args.http_upgrade_path_prefix
        };

        let transport_scheme = TransportScheme::from_str(args.remote_addr.scheme())
            .expect("invalid scheme in server url");
        let tls = match transport_scheme {
            TransportScheme::Ws | TransportScheme::Http => None,
            TransportScheme::Wss | TransportScheme::Https => Some(TlsClientConfig {
                tls_connector: Arc::new(RwLock::new(
                    tls::tls_connector(
                        args.tls_verify_certificate,
                        transport_scheme.alpn_protocols(),
                        !args.tls_sni_disable,
                        tls_certificate,
                        tls_key,
                    )
                    .expect("Cannot create tls connector"),
                )),
                tls_sni_override: args.tls_sni_override,
                tls_verify_certificate: args.tls_verify_certificate,
                tls_sni_disabled: args.tls_sni_disable,
                tls_certificate_path: args.tls_certificate.clone(),
                tls_key_path: args.tls_private_key.clone(),
            }),
        };

        // Extract host header from http_headers
        let host_header =
            if let Some((_, host_val)) = args.http_headers.iter().find(|(h, _)| *h == HOST) {
                host_val.clone()
            } else {
                let host = match args.remote_addr.port_or_known_default() {
                    None | Some(80) | Some(443) => args.remote_addr.host().unwrap().to_string(),
                    Some(port) => format!("{}:{}", args.remote_addr.host().unwrap(), port),
                };
                HeaderValue::from_str(&host)?
            };
        if let Some(path) = &args.http_headers_file {
            if !path.exists() {
                panic!("http headers file does not exists: {}", path.display());
            }
        }

        let http_proxy = Self::mk_http_proxy(
            args.http_proxy,
            args.http_proxy_login,
            args.http_proxy_password,
        )?;
        let client_config = WsClientConfig {
            remote_addr: TransportAddr::new(
                TransportScheme::from_str(args.remote_addr.scheme()).unwrap(),
                args.remote_addr.host().unwrap().to_owned(),
                args.remote_addr.port_or_known_default().unwrap(),
                tls,
            )
            .unwrap(),
            socket_so_mark: args.socket_so_mark,
            http_upgrade_path_prefix,
            http_upgrade_credentials: args.http_upgrade_credentials,
            http_headers: args
                .http_headers
                .into_iter()
                .filter(|(k, _)| k != HOST)
                .collect(),
            http_headers_file: args.http_headers_file,
            http_header_host: host_header,
            timeout_connect: Duration::from_secs(10),
            websocket_ping_frequency: args
                .websocket_ping_frequency_sec
                .or(Some(Duration::from_secs(30)))
                .filter(|d| d.as_secs() > 0),
            websocket_mask_frame: args.websocket_mask_frame,
            dns_resolver: DnsResolver::new_from_urls(
                &args.dns_resolver,
                http_proxy.clone(),
                args.socket_so_mark,
                !args.dns_resolver_prefer_ipv4,
            )
            .expect("cannot create dns resolver"),
            http_proxy,
        };

        let client = WsClient::new(
            client_config,
            args.connection_min_idle,
            args.connection_retry_max_backoff_sec,
        )
        .await?;
        info!("Starting wstunnel client v{}", env!("CARGO_PKG_VERSION"),);

        // Start tunnels
        for tunnel in args.remote_to_local.into_iter() {
            let client = client.clone();
            match &tunnel.local_protocol {
                LocalProtocol::ReverseTcp { .. } => {
                    tokio::spawn(async move {
                        let cfg = client.config.clone();
                        let tcp_connector = TcpTunnelConnector::new(
                            &tunnel.remote.0,
                            tunnel.remote.1,
                            cfg.socket_so_mark,
                            cfg.timeout_connect,
                            &cfg.dns_resolver,
                        );
                        let (host, port) = to_host_port(tunnel.local);
                        let remote = RemoteAddr {
                            protocol: LocalProtocol::ReverseTcp,
                            host,
                            port,
                        };
                        if let Err(err) = client.run_reverse_tunnel(remote, tcp_connector).await {
                            error!("{:?}", err);
                        }
                    });
                }
                LocalProtocol::ReverseUdp { timeout } => {
                    let timeout = *timeout;

                    tokio::spawn(async move {
                        let cfg = client.config.clone();
                        let (host, port) = to_host_port(tunnel.local);
                        let remote = RemoteAddr {
                            protocol: LocalProtocol::ReverseUdp { timeout },
                            host,
                            port,
                        };
                        let udp_connector = UdpTunnelConnector::new(
                            &remote.host,
                            remote.port,
                            cfg.socket_so_mark,
                            cfg.timeout_connect,
                            &cfg.dns_resolver,
                        );

                        if let Err(err) = client
                            .run_reverse_tunnel(remote.clone(), udp_connector)
                            .await
                        {
                            error!("{:?}", err);
                        }
                    });
                }
                LocalProtocol::ReverseSocks5 {
                    timeout,
                    credentials,
                } => {
                    let credentials = credentials.clone();
                    let timeout = *timeout;
                    tokio::spawn(async move {
                        let cfg = client.config.clone();
                        let (host, port) = to_host_port(tunnel.local);
                        let remote = RemoteAddr {
                            protocol: LocalProtocol::ReverseSocks5 {
                                timeout,
                                credentials,
                            },
                            host,
                            port,
                        };
                        let socks_connector = Socks5TunnelConnector::new(
                            cfg.socket_so_mark,
                            cfg.timeout_connect,
                            &cfg.dns_resolver,
                        );

                        if let Err(err) = client.run_reverse_tunnel(remote, socks_connector).await {
                            error!("{:?}", err);
                        }
                    });
                }
                LocalProtocol::ReverseHttpProxy {
                    timeout,
                    credentials,
                } => {
                    let credentials = credentials.clone();
                    let timeout = *timeout;
                    tokio::spawn(async move {
                        let cfg = client.config.clone();
                        let (host, port) = to_host_port(tunnel.local);
                        let remote = RemoteAddr {
                            protocol: LocalProtocol::ReverseHttpProxy {
                                timeout,
                                credentials,
                            },
                            host,
                            port,
                        };
                        let tcp_connector = TcpTunnelConnector::new(
                            &remote.host,
                            remote.port,
                            cfg.socket_so_mark,
                            cfg.timeout_connect,
                            &cfg.dns_resolver,
                        );

                        if let Err(err) = client
                            .run_reverse_tunnel(remote.clone(), tcp_connector)
                            .await
                        {
                            error!("{:?}", err);
                        }
                    });
                }
                LocalProtocol::ReverseUnix { path } => {
                    let path = path.clone();
                    tokio::spawn(async move {
                        let cfg = client.config.clone();
                        let tcp_connector = TcpTunnelConnector::new(
                            &tunnel.remote.0,
                            tunnel.remote.1,
                            cfg.socket_so_mark,
                            cfg.timeout_connect,
                            &cfg.dns_resolver,
                        );

                        let (host, port) = to_host_port(tunnel.local);
                        let remote = RemoteAddr {
                            protocol: LocalProtocol::ReverseUnix { path },
                            host,
                            port,
                        };
                        if let Err(err) = client.run_reverse_tunnel(remote, tcp_connector).await {
                            error!("{:?}", err);
                        }
                    });
                }
                LocalProtocol::Stdio { .. }
                | LocalProtocol::TProxyTcp
                | LocalProtocol::TProxyUdp { .. }
                | LocalProtocol::Tcp { .. }
                | LocalProtocol::Udp { .. }
                | LocalProtocol::Socks5 { .. }
                | LocalProtocol::HttpProxy { .. } => {}
                LocalProtocol::Unix { .. } => {
                    panic!("Invalid protocol for reverse tunnel");
                }
            }
        }

        for tunnel in args.local_to_remote.into_iter() {
            let client = client.clone();

            match &tunnel.local_protocol {
                LocalProtocol::Tcp { proxy_protocol } => {
                    let server = TcpTunnelListener::new(
                        tunnel.local,
                        tunnel.remote.clone(),
                        *proxy_protocol,
                    )
                    .await?;
                    tokio::spawn(async move {
                        if let Err(err) = client.run_tunnel(server).await {
                            error!("{:?}", err);
                        }
                    });
                }
                #[cfg(target_os = "linux")]
                LocalProtocol::TProxyTcp => {
                    use crate::tunnel::listeners::TproxyTcpTunnelListener;
                    let server = TproxyTcpTunnelListener::new(tunnel.local, false).await?;

                    tokio::spawn(async move {
                        if let Err(err) = client.run_tunnel(server).await {
                            error!("{:?}", err);
                        }
                    });
                }
                #[cfg(unix)]
                LocalProtocol::Unix {
                    path,
                    proxy_protocol,
                } => {
                    use crate::tunnel::listeners::UnixTunnelListener;
                    let server =
                        UnixTunnelListener::new(path, tunnel.remote.clone(), *proxy_protocol)
                            .await?;
                    tokio::spawn(async move {
                        if let Err(err) = client.run_tunnel(server).await {
                            error!("{:?}", err);
                        }
                    });
                }
                #[cfg(not(unix))]
                LocalProtocol::Unix { .. } => {
                    panic!("Unix socket is not available for non Unix platform")
                }

                #[cfg(target_os = "linux")]
                LocalProtocol::TProxyUdp { timeout } => {
                    use crate::tunnel::listeners::new_tproxy_udp;
                    let server = new_tproxy_udp(tunnel.local, *timeout).await?;
                    tokio::spawn(async move {
                        if let Err(err) = client.run_tunnel(server).await {
                            error!("{:?}", err);
                        }
                    });
                }
                #[cfg(not(target_os = "linux"))]
                LocalProtocol::TProxyTcp | LocalProtocol::TProxyUdp { .. } => {
                    panic!("Transparent proxy is not available for non Linux platform")
                }
                LocalProtocol::Udp { timeout } => {
                    let server =
                        UdpTunnelListener::new(tunnel.local, tunnel.remote.clone(), *timeout)
                            .await?;

                    tokio::spawn(async move {
                        if let Err(err) = client.run_tunnel(server).await {
                            error!("{:?}", err);
                        }
                    });
                }
                LocalProtocol::Socks5 {
                    timeout,
                    credentials,
                } => {
                    let server =
                        Socks5TunnelListener::new(tunnel.local, *timeout, credentials.clone())
                            .await?;
                    tokio::spawn(async move {
                        if let Err(err) = client.run_tunnel(server).await {
                            error!("{:?}", err);
                        }
                    });
                }
                LocalProtocol::HttpProxy {
                    timeout,
                    credentials,
                    proxy_protocol,
                } => {
                    let server = HttpProxyTunnelListener::new(
                        tunnel.local,
                        *timeout,
                        credentials.clone(),
                        *proxy_protocol,
                    )
                    .await?;
                    tokio::spawn(async move {
                        if let Err(err) = client.run_tunnel(server).await {
                            error!("{:?}", err);
                        }
                    });
                }

                LocalProtocol::Stdio { proxy_protocol } => {
                    let (server, mut handle) =
                        new_stdio_listener(tunnel.remote.clone(), *proxy_protocol).await?;
                    tokio::spawn(async move {
                        if let Err(err) = client.run_tunnel(server).await {
                            error!("{:?}", err);
                        }
                    });

                    // We need to wait for either a ctrl+c of that the stdio tunnel is closed
                    // to force exit the program
                    select! {
                       _ = handle.closed() => {},
                       _ = tokio::signal::ctrl_c() => {}
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    std::process::exit(0);
                }
                LocalProtocol::ReverseTcp => {}
                LocalProtocol::ReverseUdp { .. } => {}
                LocalProtocol::ReverseSocks5 { .. } => {}
                LocalProtocol::ReverseUnix { .. } => {}
                LocalProtocol::ReverseHttpProxy { .. } => {}
            }
        }
        Ok(())
    }

    fn mk_http_proxy(
        http_proxy: Option<String>,
        proxy_login: Option<String>,
        proxy_password: Option<String>,
    ) -> anyhow::Result<Option<Url>> {
        let Some(proxy) = http_proxy else {
            return Ok(None);
        };

        let mut proxy = if proxy.starts_with("http://") {
            Url::parse(&proxy).with_context(|| "Invalid http proxy url")?
        } else {
            Url::parse(&format!("http://{}", proxy)).with_context(|| "Invalid http proxy url")?
        };

        if let Some(login) = proxy_login {
            proxy
                .set_username(login.as_str())
                .map_err(|_| anyhow!("Cannot set http proxy login"))?;
        }

        if let Some(password) = proxy_password {
            proxy
                .set_password(Some(password.as_str()))
                .map_err(|_| anyhow!("Cannot set http proxy password"))?;
        }

        Ok(Some(proxy))
    }
}

#[derive(Debug)]
struct Client {
    /// Listen on local and forwards traffic from remote. Can be specified multiple times
    /// examples:
    /// 'tcp://1212:google.com:443'      =>       listen locally on tcp on port 1212 and forward to google.com on port 443
    /// 'tcp://2:n.lan:4?proxy_protocol' =>       listen locally on tcp on port 2 and forward to n.lan on port 4
    ///                                           Send a proxy protocol header v2 when establishing connection to n.lan
    ///
    /// 'udp://1212:1.1.1.1:53'          =>       listen locally on udp on port 1212 and forward to cloudflare dns 1.1.1.1 on port 53
    /// 'udp://1212:1.1.1.1:53?timeout_sec=10'    timeout_sec on udp force close the tunnel after 10sec. Set it to 0 to disable the timeout [default: 30]
    ///
    /// 'socks5://[::1]:1212'            =>       listen locally with socks5 on port 1212 and forward dynamically requested tunnel
    /// 'socks5://[::1]:1212?login=admin&password=admin' => listen locally with socks5 on port 1212 and only accept connection with login=admin and password=admin
    ///
    /// 'http://[::1]:1212'              =>       start a http proxy on port 1212 and forward dynamically requested tunnel
    /// 'http://[::1]:1212?login=admin&password=admin' => start a http proxy on port 1212 and only accept connection with login=admin and password=admin
    ///
    /// 'tproxy+tcp://[::1]:1212'        =>       listen locally on tcp on port 1212 as a *transparent proxy* and forward dynamically requested tunnel
    /// 'tproxy+udp://[::1]:1212?timeout_sec=10'  listen locally on udp on port 1212 as a *transparent proxy* and forward dynamically requested tunnel
    ///                                           linux only and requires sudo/CAP_NET_ADMIN
    ///
    /// 'stdio://google.com:443'         =>       listen for data from stdio, mainly for `ssh -o ProxyCommand="wstunnel client -L stdio://%h:%p ws://localhost:8080" my-server`
    ///
    /// 'unix:///tmp/wstunnel.sock:g.com:443' =>  listen for data from unix socket of path /tmp/wstunnel.sock and forward to g.com:443
    local_to_remote: Vec<LocalToRemote>,

    /// Listen on remote and forwards traffic from local. Can be specified multiple times. Only tcp is supported
    /// examples:
    /// 'tcp://1212:google.com:443'      =>     listen on server for incoming tcp cnx on port 1212 and forward to google.com on port 443 from local machine
    /// 'udp://1212:1.1.1.1:53'          =>     listen on server for incoming udp on port 1212 and forward to cloudflare dns 1.1.1.1 on port 53 from local machine
    /// 'socks5://[::1]:1212'            =>     listen on server for incoming socks5 request on port 1212 and forward dynamically request from local machine (login/password is supported)
    /// 'http://[::1]:1212'         =>     listen on server for incoming http proxy request on port 1212 and forward dynamically request from local machine (login/password is supported)
    /// 'unix://wstunnel.sock:g.com:443' =>     listen on server for incoming data from unix socket of path wstunnel.sock and forward to g.com:443 from local machine
    remote_to_local: Vec<LocalToRemote>,

    /// (linux only) Mark network packet with SO_MARK sockoption with the specified value.
    /// You need to use {root, sudo, capabilities} to run wstunnel when using this option
    socket_so_mark: Option<u32>,

    /// Client will maintain a pool of open connection to the server, in order to speed up the connection process.
    /// This option set the maximum number of connection that will be kept open.
    /// This is useful if you plan to create/destroy a lot of tunnel (i.e: with socks5 to navigate with a browser)
    /// It will avoid the latency of doing tcp + tls handshake with the server
    connection_min_idle: u32,

    /// The maximum of time in seconds while we are going to try to connect to the server before failing the connection/tunnel request
    connection_retry_max_backoff_sec: Duration,

    /// Domain name that will be used as SNI during TLS handshake
    /// Warning: If you are behind a CDN (i.e: Cloudflare) you must set this domain also in the http HOST header.
    ///          or it will be flagged as fishy and your request rejected
    tls_sni_override: Option<DnsName<'static>>,

    /// Disable sending SNI during TLS handshake
    /// Warning: Most reverse proxies rely on it
    tls_sni_disable: bool,

    /// Enable TLS certificate verification.
    /// Disabled by default. The client will happily connect to any server with self-signed certificate.
    tls_verify_certificate: bool,

    /// If set, will use this http proxy to connect to the server
    http_proxy: Option<String>,

    /// If set, will use this login to connect to the http proxy. Override the one from --http-proxy
    http_proxy_login: Option<String>,

    /// If set, will use this password to connect to the http proxy. Override the one from --http-proxy
    http_proxy_password: Option<String>,

    /// Use a specific prefix that will show up in the http path during the upgrade request.
    /// Useful if you need to route requests server side but don't have vhosts
    /// When using mTLS this option overrides the default behavior of using the common name of the
    /// client's certificate. This will likely result in the wstunnel server rejecting the connection.
    http_upgrade_path_prefix: String,

    /// Pass authorization header with basic auth credentials during the upgrade request.
    /// If you need more customization, you can use the http_headers option.
    http_upgrade_credentials: Option<HeaderValue>,

    /// Frequency at which the client will send websocket pings to the server.
    /// Set to zero to disable.
    websocket_ping_frequency_sec: Option<Duration>,

    /// Enable the masking of websocket frames. Default is false
    /// Enable this option only if you use unsecure (non TLS) websocket server, and you see some issues. Otherwise, it is just overhead.
    websocket_mask_frame: bool,

    /// Send custom headers in the upgrade request
    /// Can be specified multiple time
    http_headers: Vec<(HeaderName, HeaderValue)>,

    /// Send custom headers in the upgrade request reading them from a file.
    /// It overrides http_headers specified from command line.
    /// File is read everytime and file format must contain lines with `HEADER_NAME: HEADER_VALUE`
    http_headers_file: Option<PathBuf>,

    /// Address of the wstunnel server
    /// You can either use websocket or http2 as transport protocol. Use websocket if you are unsure.
    /// Example: For websocket with TLS wss://wstunnel.example.com or without ws://wstunnel.example.com
    ///          For http2 with TLS https://wstunnel.example.com or without http://wstunnel.example.com
    ///
    /// *WARNING* HTTP2 as transport protocol is harder to make it works because:
    ///   - If you are behind a (reverse) proxy/CDN they are going to buffer the whole request before forwarding it to the server
    ///     Obviously, this is not going to work for tunneling traffic
    ///   - if you have wstunnel behind a reverse proxy, most of them (i.e: nginx) are going to turn http2 request into http1
    ///     This is not going to work, because http1 does not support streaming naturally
    ///   - The only way to make it works with http2 is to have wstunnel directly exposed to the internet without any reverse proxy in front of it
    remote_addr: Url,

    /// [Optional] Certificate (pem) to present to the server when connecting over TLS (HTTPS).
    /// Used when the server requires clients to authenticate themselves with a certificate (i.e. mTLS).
    /// Unless overridden, the HTTP upgrade path will be configured to be the common name (CN) of the certificate.
    /// The certificate will be automatically reloaded if it changes
    tls_certificate: Option<PathBuf>,

    /// [Optional] The private key for the corresponding certificate used with mTLS.
    /// The certificate will be automatically reloaded if it changes
    tls_private_key: Option<PathBuf>,

    /// Dns resolver to use to lookup ips of domain name. Can be specified multiple time
    /// Example:
    ///  dns://1.1.1.1 for using udp
    ///  dns+https://1.1.1.1?sni=cloudflare-dns.com for using dns over HTTPS
    ///  dns+tls://8.8.8.8?sni=dns.google for using dns over TLS
    /// For Dns over HTTPS/TLS if an HTTP proxy is configured, it will be used also
    /// To use libc resolver, use
    /// system://0.0.0.0
    ///
    /// **WARN** On windows you may want to specify explicitly the DNS resolver to avoid excessive DNS queries
    dns_resolver: Vec<Url>,

    /// Enable if you prefer the dns resolver to prioritize IPv4 over IPv6
    /// This is useful if you have a broken IPv6 connection, and want to avoid the delay of trying to connect to IPv6
    /// If you don't have any IPv6 this does not change anything.
    dns_resolver_prefer_ipv4: bool,
}

#[derive(Clone, Debug)]
pub struct LocalToRemote {
    local_protocol: LocalProtocol,
    local: SocketAddr,
    remote: (Host, u16),
}
