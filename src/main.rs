//! Command line arguments.
use std::{
    fs, io,
    net::{SocketAddr, SocketAddrV4, SocketAddrV6, ToSocketAddrs},
    path::PathBuf,
    str::FromStr,
    sync::LazyLock,
    time::Duration,
};

use anyhow::{bail, ensure, Context, Result};
use clap::{Parser, Subcommand};
use dumbpipe::NodeTicket;
#[cfg(unix)]
use iroh::endpoint::Connection;
use iroh::{
    endpoint::{Incoming, RecvStream, SendStream},
    Endpoint, NodeAddr, RelayMap, RelayMode, RelayUrl, SecretKey,
};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    select,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const ONLINE_TIMEOUT: Duration = Duration::from_secs(5);

/// Name of the file, in the directory where the program is run, used to
/// persist the secret key for reuse across invocations.
const SECRET_FILE_NAME: &str = "iroh-secret.txt";

/// Default relay servers used when `--relay` is not specified.
///
/// These are self-hosted chatmail/RU relays (mostly Delta Chat chatmail
/// servers) that speak the plain WebSocket relay protocol, instead of the
/// built-in n0 relay preset that ships with iroh, which the iroh 0.35 relay
/// client can no longer talk to (HTTP 400).
const DEFAULT_RELAYS: &[&str] = &[
    "https://nine.testrun.org",
    "https://mehl.cloud",
    "https://mailchat.pl",
    "https://chatmail.woodpeckersnest.space",
    "https://chatmail.culturanerd.it",
    "https://chat.adminforge.de",
    "https://chika.aangat.lahat.computer",
    "https://tarpit.fun",
    "https://d.gaufr.es",
    "https://chtml.ca",
    "https://e2ee.wang",
    "https://chat.privittytech.com",
    "https://e2ee.im",
    "https://chatmail.email",
    "https://chat.in-the.eu",
    "https://chat.nuvon.app",
    "https://nibblehole.com",
    "https://chat.zashm.org",
    "https://chat.sus.fr",
    "https://delta.thelab.uno",
    "https://chat.vim.wtf",
    "https://uninterest.ing",
    "https://sweetfern.net",
    "https://delta.disobey.net",
    "https://chat.gluek.info",
    "https://chatmail.uk",
    "https://arcanechat.me",
    "https://dnd.wb.ru",
    "https://talklink.fun",
    "https://cm.dc09.xyz",
    "https://deltachat.kz",
    "https://cm1.wwire.su",
    "https://msk.ru.deltachat.fans",
    "https://krsk.ru.deltachat.fans",
    "https://se.deltachat.fans",
    "https://chat.ourpeering.cc",
    "https://chat.tatars.cc",
    "https://chat.bashkort.cc",
    "https://chat.chudppl.org",
    "https://chat.pdvkn.ru",
    "https://cm4.project26.cc",
];

/// The relay map built from [`DEFAULT_RELAYS`].
static DEFAULT_RELAY_MAP: LazyLock<RelayMap> = LazyLock::new(|| {
    RelayMap::from_iter(
        DEFAULT_RELAYS
            .iter()
            .map(|url| RelayUrl::from_str(url).expect("invalid default relay url")),
    )
});

/// Create a dumb pipe between two machines, using an iroh endpoint.
///
/// One side listens, the other side connects. Both sides are identified by a
/// 32 byte endpoint id.
///
/// Connecting to a endpoint id is independent of its IP address. Dumbpipe will try
/// to establish a direct connection even through NATs and firewalls. If that
/// fails, it will fall back to using a relay server.
///
/// For all subcommands, you can specify a secret key using the IROH_SECRET
/// environment variable. If you don't, the secret saved in `iroh-secret.txt`
/// in the current directory is reused. If there is no such file either, a
/// random one will be generated.
///
/// You can also specify a port for the endpoint. If you don't, a random one
/// will be chosen.
#[derive(Parser, Debug)]
pub struct Args {
    #[clap(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Generate a short endpoint ticket. This ticket can be used to later connect to a
    /// listener that is using the same secret key again.
    ///
    /// This command only really makes sense when you are providing dumbpipe with a
    /// secret key.
    GenerateTicket,

    /// Generate a fresh secret key, save it to `iroh-secret.txt` in the current
    /// directory, and print a ticket for it. Future runs without IROH_SECRET
    /// will reuse the saved secret, so the ticket can be used to connect to a
    /// listener running in the same directory.
    SaveTicket,

    /// Listen on an endpoint and forward stdin/stdout to the first incoming
    /// bidi stream.
    ///
    /// Will print a endpoint ticket on stderr that can be used to connect.
    Listen(ListenArgs),

    /// Listen on an endpoint and forward incoming connections to the specified
    /// host and port. Every incoming bidi stream is forwarded to a new connection.
    ///
    /// Will print a endpoint ticket on stderr that can be used to connect.
    ///
    /// As far as the endpoint is concerned, this is listening. But it is
    /// connecting to a TCP socket for which you have to specify the host and port.
    ListenTcp(ListenTcpArgs),

    /// Connect to an endpoint, open a bidi stream, and forward stdin/stdout.
    ///
    /// A endpoint ticket is required to connect.
    Connect(ConnectArgs),

    /// Connect to an endpoint, open a bidi stream, and forward stdin/stdout
    /// to it.
    ///
    /// A endpoint ticket is required to connect.
    ///
    /// As far as the endpoint is concerned, this is connecting. But it is
    /// listening on a TCP socket for which you have to specify the interface and port.
    ConnectTcp(ConnectTcpArgs),

    #[cfg(unix)]
    /// Listen on an endpoint and forward incoming connections to the specified
    /// Unix socket path. Every incoming bidi stream is forwarded to a new connection.
    ///
    /// Will print a endpoint ticket on stderr that can be used to connect.
    ///
    /// As far as the endpoint is concerned, this is listening. But it is
    /// connecting to a Unix socket for which you have to specify the path.
    ListenUnix(ListenUnixArgs),

    #[cfg(unix)]
    /// Connect to an endpoint, open a bidi stream, and forward connections
    /// from the specified Unix socket path.
    ///
    /// A endpoint ticket is required to connect.
    ///
    /// As far as the endpoint is concerned, this is connecting. But it is
    /// listening on a Unix socket for which you have to specify the path.
    ConnectUnix(ConnectUnixArgs),
}

#[derive(Parser, Debug)]
pub struct CommonArgs {
    /// The IPv4 address that the endpoint will listen on.
    ///
    /// If None, defaults to a random free port, but it can be useful to specify a fixed
    /// port, e.g. to configure a firewall rule.
    #[clap(long, default_value = None)]
    pub ipv4_addr: Option<SocketAddrV4>,

    /// The IPv6 address that the endpoint will listen on.
    ///
    /// If None, defaults to a random free port, but it can be useful to specify a fixed
    /// port, e.g. to configure a firewall rule.
    #[clap(long, default_value = None)]
    pub ipv6_addr: Option<SocketAddrV6>,

    /// A custom ALPN to use for the endpoint.
    ///
    /// This is an expert feature that allows dumbpipe to be used to interact
    /// with existing iroh protocols.
    ///
    /// When using this option, the connect side must also specify the same ALPN.
    /// The listen side will not expect a handshake, and the connect side will
    /// not send one.
    ///
    /// Alpns are byte strings. To specify an utf8 string, prefix it with `utf8:`.
    /// Otherwise, it will be parsed as a hex string.
    #[clap(long)]
    pub custom_alpn: Option<String>,

    /// Use only the specified relay, replacing the default n0 relays
    /// (e.g. https://use1-1.relay.n0.iroh.link).
    ///
    /// Direct / hole-punching connection attempts are still made.
    #[clap(short = 'r', long)]
    pub relay: Option<RelayUrl>,

    /// Only use the relay transport: do not listen for or initiate direct
    /// (hole punching) connections. All traffic goes through a relay.
    #[clap(long)]
    pub no_direct: bool,

    /// The verbosity level. Repeat to increase verbosity.
    #[clap(short = 'v', long, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

impl CommonArgs {
    fn alpn(&self) -> Result<Vec<u8>> {
        Ok(match &self.custom_alpn {
            Some(alpn) => parse_alpn(alpn)?,
            None => dumbpipe::ALPN.to_vec(),
        })
    }

    fn is_custom_alpn(&self) -> bool {
        self.custom_alpn.is_some()
    }
}

fn parse_alpn(alpn: &str) -> Result<Vec<u8>> {
    Ok(if let Some(text) = alpn.strip_prefix("utf8:") {
        text.as_bytes().to_vec()
    } else {
        hex::decode(alpn).context("invalid alpn")?
    })
}

#[derive(Parser, Debug)]
pub struct ListenArgs {
    /// Immediately close our sending side, indicating that we will not transmit any data
    #[clap(long)]
    pub recv_only: bool,

    #[clap(flatten)]
    pub common: CommonArgs,
}

#[derive(Parser, Debug)]
pub struct ListenTcpArgs {
    #[clap(long)]
    pub host: String,

    #[clap(flatten)]
    pub common: CommonArgs,
}

#[derive(Parser, Debug)]
pub struct ConnectTcpArgs {
    /// The addresses to listen on for incoming tcp connections.
    ///
    /// To listen on all network interfaces, use 0.0.0.0:12345
    #[clap(long)]
    pub addr: String,

    /// The endpoint to connect to
    pub ticket: NodeTicket,

    #[clap(flatten)]
    pub common: CommonArgs,
}

#[derive(Parser, Debug)]
pub struct ConnectArgs {
    /// The endpoint to connect to
    pub ticket: NodeTicket,

    /// Immediately close our sending side, indicating that we will not transmit any data
    #[clap(long)]
    pub recv_only: bool,

    #[clap(flatten)]
    pub common: CommonArgs,
}

#[cfg(unix)]
#[derive(Parser, Debug)]
pub struct ListenUnixArgs {
    /// Path to the Unix socket to connect to
    #[clap(long)]
    pub socket_path: PathBuf,

    #[clap(flatten)]
    pub common: CommonArgs,
}

#[cfg(unix)]
#[derive(Parser, Debug)]
pub struct ConnectUnixArgs {
    /// Path to the Unix socket to listen on
    #[clap(long)]
    pub socket_path: PathBuf,

    /// The endpoint to connect to
    pub ticket: NodeTicket,

    #[clap(flatten)]
    pub common: CommonArgs,
}

/// Copy from a reader to a noq stream.
///
/// Will send a reset to the other side if the operation is cancelled, and fail
/// with an error.
///
/// Returns the number of bytes copied in case of success.
async fn copy_to_noq(
    mut from: impl AsyncRead + Unpin,
    mut send: SendStream,
    token: CancellationToken,
) -> io::Result<u64> {
    tracing::trace!("copying to noq");
    tokio::select! {
        res = tokio::io::copy(&mut from, &mut send) => {
            let size = res?;
            send.finish()?;
            Ok(size)
        }
        _ = token.cancelled() => {
            // send a reset to the other side immediately
            send.reset(0u8.into()).ok();
            Err(io::Error::other("cancelled"))
        }
    }
}

/// Copy from a noq stream to a writer.
///
/// Will send stop to the other side if the operation is cancelled, and fail
/// with an error.
///
/// Returns the number of bytes copied in case of success.
async fn copy_from_noq(
    mut recv: RecvStream,
    mut to: impl AsyncWrite + Unpin,
    token: CancellationToken,
) -> io::Result<u64> {
    tokio::select! {
        res = tokio::io::copy(&mut recv, &mut to) => {
            Ok(res?)
        },
        _ = token.cancelled() => {
            recv.stop(0u8.into()).ok();
            Err(io::Error::other("cancelled"))
        }
    }
}

/// Path to the secret file in the directory where the program is run.
fn secret_file_path() -> Result<PathBuf> {
    let dir = std::env::current_dir().context("could not determine current directory")?;
    Ok(dir.join(SECRET_FILE_NAME))
}

/// Read the secret key from SECRET_FILE_NAME in the current directory, if it exists.
fn read_secret_from_file() -> Result<Option<SecretKey>> {
    let path = secret_file_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read secret file {}", path.display()))?;
    let secret = SecretKey::from_str(contents.trim())
        .with_context(|| format!("invalid secret in file {}", path.display()))?;
    Ok(Some(secret))
}

/// Write the secret key to SECRET_FILE_NAME in the current directory,
/// with 0600 permissions on unix.
fn write_secret_file(secret: &SecretKey) -> Result<()> {
    let path = secret_file_path()?;
    fs::write(&path, format!("{}\n", secret))
        .with_context(|| format!("failed to write secret file {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).with_context(|| {
            format!(
                "failed to set permissions on secret file {}",
                path.display()
            )
        })?;
    }
    Ok(())
}

/// Manufacture a new secret key, printing it to stderr so the user can save it.
fn new_secret() -> SecretKey {
    let key = SecretKey::generate(rand::rngs::OsRng);
    eprintln!(
        "using secret key {}",
        data_encoding::HEXLOWER.encode(&key.to_bytes())
    );
    key
}

/// Get the secret key or generate a new one.
///
/// The `IROH_SECRET` environment variable takes priority, then a secret saved
/// in SECRET_FILE_NAME in the current directory is reused. Print the secret
/// key to stderr if it was generated, so the user can save it.
fn get_or_create_secret() -> Result<SecretKey> {
    match std::env::var("IROH_SECRET") {
        Ok(secret) => SecretKey::from_str(&secret).context("invalid secret"),
        Err(_) => match read_secret_from_file()? {
            Some(secret) => Ok(secret),
            None => Ok(new_secret()),
        },
    }
}

/// Create a new iroh endpoint.
async fn create_endpoint(
    secret_key: SecretKey,
    common: &CommonArgs,
    alpns: Vec<Vec<u8>>,
) -> Result<Endpoint> {
    let mut builder = Endpoint::builder().secret_key(secret_key).alpns(alpns);
    if let Some(relay) = &common.relay {
        builder = builder.relay_mode(RelayMode::Custom(RelayMap::from(relay.clone())));
    } else {
        builder = builder.relay_mode(RelayMode::Custom(DEFAULT_RELAY_MAP.clone()));
    }
    if let Some(addr) = common.ipv4_addr {
        builder = builder.bind_addr_v4(addr);
    }
    if let Some(addr) = common.ipv6_addr {
        builder = builder.bind_addr_v6(addr);
    }
    let endpoint = builder.bind().await.context("failed to bind endpoint")?;
    Ok(endpoint)
}

fn cancel_token<T>(token: CancellationToken) -> impl Fn(T) -> T {
    move |x| {
        token.cancel();
        x
    }
}

/// Bidirectionally forward data from a noq stream and an arbitrary tokio
/// reader/writer pair, aborting both sides when either one forwarder is done,
/// or when control-c is pressed.
async fn forward_bidi(
    from1: impl AsyncRead + Send + Sync + Unpin + 'static,
    to1: impl AsyncWrite + Send + Sync + Unpin + 'static,
    from2: RecvStream,
    to2: SendStream,
) -> Result<()> {
    let token1 = CancellationToken::new();
    let token2 = token1.clone();
    let token3 = token1.clone();
    let forward_from_stdin = tokio::spawn(async move {
        copy_to_noq(from1, to2, token1.clone())
            .await
            .map_err(cancel_token(token1))
    });
    let forward_to_stdout = tokio::spawn(async move {
        copy_from_noq(from2, to1, token2.clone())
            .await
            .map_err(cancel_token(token2))
    });
    let _control_c = tokio::spawn(async move {
        tokio::signal::ctrl_c().await?;
        token3.cancel();
        io::Result::Ok(())
    });
    forward_to_stdout
        .await
        .context("forward to stdout failed")?
        .context("forward to stdout failed")?;
    forward_from_stdin
        .await
        .context("forward from stdin failed")?
        .context("forward from stdin failed")?;
    Ok(())
}

/// Wait for the endpoint to figure out its home relay, with a timeout.
async fn wait_online(endpoint: &Endpoint) {
    let mut home_relay = endpoint.home_relay();
    if (timeout(ONLINE_TIMEOUT, home_relay.initialized()).await).is_err() {
        eprintln!("Warning: Failed to connect to the home relay");
    }
}

/// The address to advertise in a ticket.
///
/// When `no_direct` is set, direct addresses are omitted so only relay dialing
/// happens.
fn endpoint_addr(endpoint: &Endpoint, no_direct: bool) -> NodeAddr {
    let mut addr = NodeAddr::new(endpoint.node_id());
    addr.relay_url = endpoint.home_relay().get().ok().flatten();
    if !no_direct {
        if let Ok(Some(addrs)) = endpoint.direct_addresses().get() {
            addr.direct_addresses = addrs.into_iter().map(|direct| direct.addr).collect();
        }
    }
    addr
}

async fn listen_stdio(args: ListenArgs) -> Result<()> {
    let secret_key = get_or_create_secret()?;
    let endpoint = create_endpoint(secret_key, &args.common, vec![args.common.alpn()?]).await?;
    // wait for the endpoint to figure out its home relay and addresses before making a ticket
    wait_online(&endpoint).await;
    let addr = endpoint_addr(&endpoint, args.common.no_direct);
    let short = create_short_ticket(&addr);
    let ticket = NodeTicket::new(addr);

    // print the ticket on stderr so it doesn't interfere with the data itself
    //
    // note that the tests rely on the ticket being the last thing printed
    eprintln!("Listening. To connect, use:\ndumbpipe connect {ticket}");
    if args.common.verbose > 0 {
        eprintln!("or:\ndumbpipe connect {short}");
    }

    loop {
        let Some(incoming) = endpoint.accept().await else {
            break;
        };
        let connection = match incoming.await {
            Ok(connection) => connection,
            Err(cause) => {
                tracing::warn!("error accepting connection: {}", cause);
                // if accept fails, we want to continue accepting connections
                continue;
            }
        };
        let remote_endpoint_id = connection.remote_node_id().context("no peer certificate")?;
        tracing::info!("got connection from {}", remote_endpoint_id);
        let (s, mut r) = match connection.accept_bi().await {
            Ok(x) => x,
            Err(cause) => {
                tracing::warn!("error accepting stream: {}", cause);
                // if accept_bi fails, we want to continue accepting connections
                continue;
            }
        };
        tracing::info!("accepted bidi stream from {}", remote_endpoint_id);
        if !args.common.is_custom_alpn() {
            // read the handshake and verify it
            let mut buf = [0u8; dumbpipe::HANDSHAKE.len()];
            r.read_exact(&mut buf)
                .await
                .context("failed to read handshake")?;
            ensure!(buf == dumbpipe::HANDSHAKE, "invalid handshake");
        }
        if args.recv_only {
            tracing::info!(
                "forwarding stdout to {} (ignoring stdin)",
                remote_endpoint_id
            );
            forward_bidi(tokio::io::empty(), tokio::io::stdout(), r, s).await?;
        } else {
            tracing::info!("forwarding stdin/stdout to {}", remote_endpoint_id);
            forward_bidi(tokio::io::stdin(), tokio::io::stdout(), r, s).await?;
        }
        // stop accepting connections after the first successful one
        break;
    }
    endpoint.close().await;
    Ok(())
}

async fn connect_stdio(args: ConnectArgs) -> Result<()> {
    let secret_key = get_or_create_secret()?;
    let endpoint = create_endpoint(secret_key, &args.common, vec![]).await?;
    let addr = dial_addr(&args.ticket, args.common.no_direct);
    let remote_endpoint_id = addr.node_id;
    // connect to the remote, try only once
    let connection = endpoint
        .connect(addr.clone(), &args.common.alpn()?)
        .await
        .context("failed to connect")?;
    tracing::info!("connected to {}", remote_endpoint_id);
    // open a bidi stream, try only once
    let (mut s, r) = connection
        .open_bi()
        .await
        .context("failed to open bidi stream")?;
    tracing::info!("opened bidi stream to {}", remote_endpoint_id);
    // send the handshake unless we are using a custom alpn
    // when using a custom alpn, everything is up to the user
    if !args.common.is_custom_alpn() {
        // the connecting side must write first. we don't know if there will be something
        // on stdin, so just write a handshake.
        s.write_all(&dumbpipe::HANDSHAKE)
            .await
            .context("failed to write handshake")?;
    }
    if args.recv_only {
        tracing::info!(
            "forwarding stdout to {} (ignoring stdin)",
            remote_endpoint_id
        );
        forward_bidi(tokio::io::empty(), tokio::io::stdout(), r, s).await?;
    } else {
        tracing::info!("forwarding stdin/stdout to {}", remote_endpoint_id);
        forward_bidi(tokio::io::stdin(), tokio::io::stdout(), r, s).await?;
    }
    tokio::io::stdout()
        .flush()
        .await
        .context("failed to flush stdout")?;
    endpoint.close().await;
    Ok(())
}

/// Listen on a tcp port and forward incoming connections to an endpoint.
async fn connect_tcp(args: ConnectTcpArgs) -> Result<()> {
    let addrs = args
        .addr
        .to_socket_addrs()
        .with_context(|| format!("invalid host string {}", args.addr))?;
    let secret_key = get_or_create_secret()?;
    let endpoint = create_endpoint(secret_key, &args.common, vec![])
        .await
        .context("unable to bind endpoint")?;
    tracing::info!("tcp listening on {:?}", addrs);

    // Wait for our own endpoint to be ready before trying to connect.
    wait_online(&endpoint).await;

    let tcp_listener = match tokio::net::TcpListener::bind(addrs.as_slice()).await {
        Ok(tcp_listener) => tcp_listener,
        Err(cause) => {
            tracing::error!("error binding tcp socket to {:?}: {}", addrs, cause);
            return Ok(());
        }
    };
    async fn handle_tcp_accept(
        next: io::Result<(tokio::net::TcpStream, SocketAddr)>,
        addr: NodeAddr,
        endpoint: Endpoint,
        handshake: bool,
        alpn: &[u8],
    ) -> Result<()> {
        let (tcp_stream, tcp_addr) = next.context("error accepting tcp connection")?;
        let (tcp_recv, tcp_send) = tcp_stream.into_split();
        tracing::info!("got tcp connection from {}", tcp_addr);
        let remote_endpoint_id = addr.node_id;
        let connection = endpoint
            .connect(addr, alpn)
            .await
            .with_context(|| format!("error connecting to {remote_endpoint_id}"))?;
        let (mut endpoint_send, endpoint_recv) = connection
            .open_bi()
            .await
            .with_context(|| format!("error opening bidi stream to {remote_endpoint_id}"))?;
        // send the handshake unless we are using a custom alpn
        // when using a custom alpn, everything is up to the user
        if handshake {
            // the connecting side must write first. we don't know if there will be something
            // on stdin, so just write a handshake.
            endpoint_send
                .write_all(&dumbpipe::HANDSHAKE)
                .await
                .context("failed to write handshake")?;
        }
        forward_bidi(tcp_recv, tcp_send, endpoint_recv, endpoint_send).await?;
        Ok(())
    }
    let addr = dial_addr(&args.ticket, args.common.no_direct);
    loop {
        // also wait for ctrl-c here so we can use it before accepting a connection
        let next = tokio::select! {
            stream = tcp_listener.accept() => stream,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("got ctrl-c, exiting");
                break;
            }
        };
        let endpoint = endpoint.clone();
        let addr = addr.clone();
        let handshake = !args.common.is_custom_alpn();
        let alpn = args.common.alpn()?;
        tokio::spawn(async move {
            if let Err(cause) = handle_tcp_accept(next, addr, endpoint, handshake, &alpn).await {
                // log error at warn level
                //
                // we should know about it, but it's not fatal
                tracing::warn!("error handling connection: {}", cause);
            }
        });
    }
    endpoint.close().await;
    Ok(())
}

/// Listen on an endpoint and forward incoming connections to a tcp socket.
async fn listen_tcp(args: ListenTcpArgs) -> Result<()> {
    let addrs = match args.host.to_socket_addrs() {
        Ok(addrs) => addrs.collect::<Vec<_>>(),
        Err(e) => bail!("invalid host string {}: {}", args.host, e),
    };
    let secret_key = get_or_create_secret()?;
    let endpoint = create_endpoint(secret_key, &args.common, vec![args.common.alpn()?]).await?;
    // wait for the endpoint to figure out its address before making a ticket
    wait_online(&endpoint).await;
    let addr = endpoint_addr(&endpoint, args.common.no_direct);
    let short = create_short_ticket(&addr);
    let ticket = NodeTicket::new(addr);

    // print the ticket on stderr so it doesn't interfere with the data itself
    //
    // note that the tests rely on the ticket being the last thing printed
    eprintln!("Forwarding incoming requests to '{}'.", args.host);
    eprintln!("To connect, use e.g.:");
    eprintln!("dumbpipe connect-tcp {ticket}");
    if args.common.verbose > 0 {
        eprintln!("or:\ndumbpipe connect-tcp {short}");
    }
    tracing::info!("endpoint id is {}", ticket.node_addr().node_id);
    tracing::info!(
        "relay url is {:?}",
        ticket
            .node_addr()
            .relay_url
            .as_ref()
            .map_or("None".to_string(), |url| url.as_str().to_string())
    );

    // handle a new incoming connection on the endpoint
    async fn handle_endpoint_accept(
        accepting: Incoming,
        addrs: Vec<std::net::SocketAddr>,
        handshake: bool,
    ) -> Result<()> {
        let connection = accepting.await.context("error accepting connection")?;
        let remote_endpoint_id = connection.remote_node_id().context("no peer certificate")?;
        tracing::info!("got connection from {}", remote_endpoint_id);
        let (s, mut r) = connection
            .accept_bi()
            .await
            .context("error accepting stream")?;
        tracing::info!("accepted bidi stream from {}", remote_endpoint_id);
        if handshake {
            // read the handshake and verify it
            let mut buf = [0u8; dumbpipe::HANDSHAKE.len()];
            r.read_exact(&mut buf)
                .await
                .context("failed to read handshake")?;
            ensure!(buf == dumbpipe::HANDSHAKE, "invalid handshake");
        }
        let connection = tokio::net::TcpStream::connect(addrs.as_slice())
            .await
            .with_context(|| format!("error connecting to {addrs:?}"))?;
        let (read, write) = connection.into_split();
        forward_bidi(read, write, r, s).await?;
        Ok(())
    }

    loop {
        let incoming = select! {
            incoming = endpoint.accept() => incoming,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("got ctrl-c, exiting");
                break;
            }
        };
        let Some(incoming) = incoming else {
            break;
        };
        let addrs = addrs.clone();
        let handshake = !args.common.is_custom_alpn();
        tokio::spawn(async move {
            if let Err(cause) = handle_endpoint_accept(incoming, addrs, handshake).await {
                // log error at warn level
                //
                // we should know about it, but it's not fatal
                tracing::warn!("error handling connection: {}", cause);
            }
        });
    }
    endpoint.close().await;
    Ok(())
}

/// Creates a ticket that only includes the node id and relay url, dropping
/// direct addresses.
fn create_short_ticket(addr: &NodeAddr) -> NodeTicket {
    let mut short = addr.clone();
    short.direct_addresses.clear();
    NodeTicket::new(short)
}

/// The address to dial from a ticket, dropping direct addresses when the
/// `--no-direct` flag is set so only relay dialing happens.
fn dial_addr(ticket: &NodeTicket, no_direct: bool) -> NodeAddr {
    let mut addr = ticket.node_addr().clone();
    if no_direct {
        addr.direct_addresses.clear();
    }
    addr
}

#[cfg(unix)]
/// Listen on an endpoint and forward incoming connections to a Unix socket.
async fn listen_unix(args: ListenUnixArgs) -> Result<()> {
    let socket_path = args.socket_path.clone();
    let secret_key = get_or_create_secret()?;
    let endpoint = create_endpoint(secret_key, &args.common, vec![args.common.alpn()?]).await?;
    // wait for the endpoint to figure out its address before making a ticket
    wait_online(&endpoint).await;
    let addr = endpoint_addr(&endpoint, args.common.no_direct);
    let short = create_short_ticket(&addr);
    let ticket = NodeTicket::new(addr);

    // print the ticket on stderr so it doesn't interfere with the data itself
    //
    // note that the tests rely on the ticket being the last thing printed
    eprintln!(
        "Forwarding incoming requests to '{}'.",
        socket_path.display()
    );
    eprintln!("To connect, use e.g.:");
    eprintln!("dumbpipe connect-unix --socket-path /path/to/client.sock {ticket}");
    eprintln!("dumbpipe connect-tcp --addr 127.0.0.1:8080 {ticket}");
    if args.common.verbose > 0 {
        eprintln!("or:\ndumbpipe connect-unix --socket-path /path/to/client.sock {short}");
        eprintln!("dumbpipe connect-tcp --addr 127.0.0.1:8080 {short}");
    }
    tracing::info!("endpoint id is {}", ticket.node_addr().node_id);
    tracing::info!(
        "relay url is {:?}",
        ticket
            .node_addr()
            .relay_url
            .as_ref()
            .map_or("None".to_string(), |url| url.as_str().to_string())
    );

    // handle a new incoming connection on the endpoint
    async fn handle_endpoint_accept(
        accepting: Incoming,
        socket_path: PathBuf,
        handshake: bool,
    ) -> Result<()> {
        tracing::trace!("accepting connection");
        let connection = accepting.await.context("error accepting connection")?;
        let remote_endpoint_id = connection.remote_node_id().context("no peer certificate")?;
        tracing::info!("got connection from {}", remote_endpoint_id);
        let (s, mut r) = connection
            .accept_bi()
            .await
            .context("error accepting stream")?;
        tracing::info!("accepted bidi stream from {}", remote_endpoint_id);
        if handshake {
            // read the handshake and verify it
            tracing::trace!("reading handshake");
            let mut buf = [0u8; dumbpipe::HANDSHAKE.len()];
            r.read_exact(&mut buf)
                .await
                .context("failed to read handshake")?;
            ensure!(buf == dumbpipe::HANDSHAKE, "invalid handshake");
            tracing::trace!("handshake verified");
        }
        tracing::trace!("connecting to backend socket {:?}", socket_path);
        let connection = UnixStream::connect(&socket_path)
            .await
            .with_context(|| format!("error connecting to {socket_path:?}"))?;
        tracing::trace!("connected to backend socket");
        let (read, write) = connection.into_split();
        tracing::trace!("starting forward_bidi");
        forward_bidi(read, write, r, s).await?;
        tracing::trace!("forward_bidi finished");
        Ok(())
    }

    loop {
        let incoming = select! {
            incoming = endpoint.accept() => incoming,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("got ctrl-c, exiting");
                break;
            }
        };
        let Some(incoming) = incoming else {
            break;
        };
        let socket_path = socket_path.clone();
        let handshake = !args.common.is_custom_alpn();
        tokio::spawn(async move {
            if let Err(cause) = handle_endpoint_accept(incoming, socket_path, handshake).await {
                // log error at warn level
                //
                // we should know about it, but it's not fatal
                tracing::warn!("error handling connection: {}", cause);
            }
        });
    }
    endpoint.close().await;
    Ok(())
}

#[cfg(unix)]
/// A RAII guard to clean up a Unix socket file.
struct UnixSocketGuard {
    path: PathBuf,
}

#[cfg(unix)]
impl Drop for UnixSocketGuard {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::error!("failed to remove socket file {:?}: {}", self.path, e);
            }
        }
    }
}

#[cfg(unix)]
/// Listen on a Unix socket and forward connections to an endpoint.
async fn connect_unix(args: ConnectUnixArgs) -> Result<()> {
    let socket_path = args.socket_path.clone();
    let secret_key = get_or_create_secret()?;
    let endpoint = create_endpoint(secret_key, &args.common, vec![])
        .await
        .context("unable to bind endpoint")?;
    tracing::info!("unix listening on {:?}", socket_path);

    // Wait for our own endpoint to be ready before trying to connect.
    wait_online(&endpoint).await;

    // Remove existing socket file if it exists
    if let Err(e) = tokio::fs::remove_file(&socket_path).await {
        if e.kind() != io::ErrorKind::NotFound {
            bail!("failed to remove existing socket file: {}", e);
        }
    }

    let addr = dial_addr(&args.ticket, args.common.no_direct);
    tracing::info!("connecting to remote endpoint: {:?}", addr);
    let connection = endpoint
        .connect(addr.clone(), &args.common.alpn()?)
        .await
        .context("failed to connect to remote endpoint")?;
    tracing::info!("connected to remote endpoint successfully");

    let unix_listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind Unix socket at {socket_path:?}"))?;
    tracing::info!("bound local unix socket: {:?}", socket_path);

    let _guard = UnixSocketGuard {
        path: socket_path.clone(),
    };

    async fn handle_unix_accept(
        next: io::Result<(UnixStream, tokio::net::unix::SocketAddr)>,
        connection: Connection,
        handshake: bool,
    ) -> Result<()> {
        tracing::trace!("handling new local connection");
        let (unix_stream, unix_addr) = next.context("error accepting unix connection")?;
        let (unix_recv, unix_send) = unix_stream.into_split();
        tracing::trace!("got unix connection from {:?}", unix_addr);

        tracing::trace!("opening bidi stream");
        let (mut endpoint_send, endpoint_recv) = connection
            .open_bi()
            .await
            .context("error opening bidi stream")?;
        tracing::trace!("bidi stream opened");

        // send the handshake unless we are using a custom alpn
        // when using a custom alpn, everything is up to the user
        if handshake {
            tracing::trace!("sending handshake");
            // the connecting side must write first. we don't know if there will be something
            // on stdin, so just write a handshake.
            endpoint_send
                .write_all(&dumbpipe::HANDSHAKE)
                .await
                .context("failed to write handshake")?;
            tracing::trace!("handshake sent");
        }

        tracing::trace!("starting forward_bidi");
        forward_bidi(unix_recv, unix_send, endpoint_recv, endpoint_send).await?;
        tracing::trace!("forward_bidi finished");
        Ok(())
    }

    tracing::info!("entering accept loop");
    loop {
        // also wait for ctrl-c here so we can use it before accepting a connection
        let next = tokio::select! {
            stream = unix_listener.accept() => stream,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("got ctrl-c, exiting");
                break;
            }
        };
        tracing::trace!("accepted a local connection");
        let connection = connection.clone();
        let handshake = !args.common.is_custom_alpn();
        tokio::spawn(async move {
            tracing::trace!("spawning handler task");
            if let Err(cause) = handle_unix_accept(next, connection, handshake).await {
                // log error at warn level
                //
                // we should know about it, but it's not fatal
                tracing::warn!("error handling connection: {}", cause);
            }
            tracing::trace!("handler task finished");
        });
    }

    endpoint.close().await;
    Ok(())
}

async fn generate_ticket() -> Result<()> {
    let secret_key = get_or_create_secret()?;
    let public_key = secret_key.public();
    let addr = NodeAddr::new(public_key);
    let ticket = NodeTicket::new(addr);
    println!("{}", ticket);
    Ok(())
}

async fn save_ticket() -> Result<()> {
    let secret_key = new_secret();
    write_secret_file(&secret_key)?;
    let public_key = secret_key.public();
    let addr = NodeAddr::new(public_key);
    let ticket = NodeTicket::new(addr);
    println!("{}", ticket);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let res = match args.command {
        Commands::GenerateTicket => generate_ticket().await,
        Commands::SaveTicket => save_ticket().await,
        Commands::Listen(args) => listen_stdio(args).await,
        Commands::ListenTcp(args) => listen_tcp(args).await,
        Commands::Connect(args) => connect_stdio(args).await,
        Commands::ConnectTcp(args) => connect_tcp(args).await,

        #[cfg(unix)]
        Commands::ListenUnix(args) => listen_unix(args).await,

        #[cfg(unix)]
        Commands::ConnectUnix(args) => connect_unix(args).await,
    };
    match res {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1)
        }
    }
}
