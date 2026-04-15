# Reth P2P Walkthrough

This guide explains the **execution-layer** P2P stack that Base uses through `reth`.

If you already read the Base consensus-layer P2P guide, the most important difference is:

- The **consensus layer** in this repo uses `libp2p`, GossipSub, and Base-specific block gossip.
- The **execution layer** mostly reuses `reth`'s Ethereum networking stack: discovery, RLPx
  sessions, `eth` requests, transaction gossip, and sync.

So the right mental model is not "Base implemented a second custom P2P stack."

The right mental model is: **Base configures and launches `reth`'s P2P stack, then relies on it
for execution-layer networking.**

## Scope

This walkthrough is about the code path behind the networking section in the
[L2 execution spec](../specs/pages/protocol/execution/index.md#networking). It follows the actual
implementation pinned by this workspace today:

- `base/base` wires the node together.
- Upstream [`reth` at `d6324d6`](https://github.com/paradigmxyz/reth/tree/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b)
  provides the real execution-layer networking implementation.

## The 30-second picture

At a high level, execution-layer P2P in `reth` looks like this:

1. Base builds a `NetworkConfig`.
2. `reth` starts discovery services (`discv4`, `discv5`, and optionally DNS discovery).
3. `reth` listens for inbound TCP connections and dials outbound peers.
4. Each connection goes through the Ethereum devp2p/RLPx handshakes.
5. Once a session is established, `reth` can:
   - gossip transactions
   - answer `eth` requests from peers
   - fetch headers and bodies from peers for sync
6. Base mostly treats that stack as a reusable component.

The key `reth` types to remember are:

- [`NetworkConfig`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/config.rs#L41-L99)
- [`NetworkManager`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/manager.rs#L106-L151)
- [`Swarm`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/swarm.rs#L49-L58)
- [`SessionManager`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/session/mod.rs)
- [`FetchClient`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/fetch/client.rs#L28-L105)

## Where Base hands off to `reth`

The best place to start in this repo is
[`BaseNetworkBuilder`](../../crates/execution/node/src/node.rs#L1039-L1130).

That type is intentionally small. It does not implement a new protocol. It mainly:

- decides whether to disable tx gossip
- decides whether to disable `discv4`
- optionally enables `discv5`
- hands the finished config to `reth::NetworkManager`

```rust
let network_builder = ctx
    .network_config_builder()?
    .apply(|mut builder| {
        let rlpx_socket = (args.addr, args.port).into();

        if disable_discovery_v4 || args.discovery.disable_discovery {
            builder = builder.disable_discv4_discovery();
        }

        if !args.discovery.disable_discovery {
            builder = builder.discovery_v5(
                args.discovery.discovery_v5_builder(
                    rlpx_socket,
                    ctx.config()
                        .network
                        .resolved_bootnodes()
                        .or_else(|| ctx.chain_spec().bootnodes())
                        .unwrap_or_default(),
                ),
            );
        }

        builder
    });

let mut network_config = ctx.build_network_config(network_builder);
network_config.tx_gossip_disabled = disable_txpool_gossip;

let network = NetworkManager::builder(network_config).await?;
let handle = ctx.start_network(network, pool);
```

What this means in plain English:

- Base uses the default `reth` network machinery.
- Base changes a few knobs that matter for rollup execution nodes.
- After that, the rest of the job belongs to `reth`.

## What goes into `NetworkConfig`

`NetworkConfig` is the object that tells `reth` how to run the network.

See [`reth_network::NetworkConfig`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/config.rs#L41-L99).

Some of the important fields are:

- `secret_key`: the node's devp2p identity
- `boot_nodes`: initial peers to try
- `discovery_v4_config` / `discovery_v5_config`: peer discovery settings
- `listener_addr`: the TCP socket for inbound RLPx connections
- `peers_config`: peer limits and peer-management rules
- `sessions_config`: session lifecycle configuration
- `status`: the `eth` status message we send during handshake
- `hello_message`: the RLPx hello message, including capabilities
- `tx_gossip_disabled`: whether the node should participate in transaction gossip
- `handshake`: the Ethereum RLPx/`eth` handshake implementation

This is the point where a lot of junior engineers have the right instinct but the wrong boundary.

It is tempting to think "this is where Base defines execution P2P behavior." That is only partly
true.

Base defines **configuration choices** here. `reth` still owns the behavior of:

- how peers are discovered
- how sessions are opened
- how `eth` messages are encoded and decoded
- how headers and bodies are fetched
- how transaction gossip runs

## How peer discovery is wired

The next layer up is `reth`'s network CLI/config plumbing.

The discovery arguments are applied in
[`DiscoveryArgs::apply_to_builder`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/node/core/src/args/network.rs#L578-L649):

```rust
if self.disable_discovery || self.disable_dns_discovery {
    network_config_builder = network_config_builder.disable_dns_discovery();
}

if self.disable_discovery || self.disable_discv4_discovery {
    network_config_builder = network_config_builder.disable_discv4_discovery();
}

if self.should_enable_discv5() {
    network_config_builder = network_config_builder
        .discovery_v5(self.discovery_v5_builder(rlpx_tcp_socket, boot_nodes));
}
```

Then the actual discovery services are created in
[`Discovery::new`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/discovery.rs#L69-L141).

That constructor can start:

- a `discv4` service
- a `discv5` service
- a DNS discovery service

It stores discovered nodes in an in-memory cache and emits `DiscoveryEvent`s that the rest of the
network stack can consume.

That is a useful detail for debugging: discovery is **not** the same thing as being connected.

Discovery only says:

- "I learned about this peer"
- "Here is an address and maybe fork metadata"

The connection and session layers still need to decide whether to dial, accept, or reject that
peer.

## What `NetworkManager` actually runs

`reth`'s own crate docs summarize the network stack as four ongoing jobs:

- discovery
- transactions
- ETH request handling
- network management

See [`docs/crates/network.md`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/docs/crates/network.md).

The main orchestration type is
[`NetworkManager`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/manager.rs#L106-L151).
It owns:

- a [`Swarm`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/swarm.rs#L49-L58)
- a shareable [`NetworkHandle`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/network.rs#L41-L48)
- channels to the transactions task
- channels to the ETH request handler

The builder path is short:

```rust
pub async fn builder<C: BlockNumReader + 'static>(
    config: NetworkConfig<C, N>,
) -> Result<NetworkBuilder<(), (), N>, NetworkError> {
    let network = Self::new(config).await?;
    Ok(network.into_builder())
}
```

And after Base passes that builder into `ctx.start_network(...)`, `reth` spawns the long-running
tasks that keep the execution-layer P2P stack alive:

- the network task itself
- the transaction task
- the ETH request handler

See [`BuilderContext::start_network_with_policies`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/node/builder/src/builder/mod.rs#L880-L931).

## `Swarm`: the glue layer

If `NetworkManager` is the top-level orchestrator, then `Swarm` is the glue.

See [`crates/net/network/src/swarm.rs`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/swarm.rs#L25-L58).

`Swarm` combines three things:

- `ConnectionListener`: accepts inbound TCP connections
- `SessionManager`: owns active and pending RLPx sessions
- `NetworkState`: tracks peers, discovery results, and request-routing state

That breakdown matters because it tells you where to look when something goes wrong:

- inbound connection problem: start with `listener.rs`
- handshake/session problem: start with `session/mod.rs`
- dial/peer-selection problem: start with `state.rs`, `peers.rs`, and `discovery.rs`

## Connection lifecycle: from TCP socket to active peer

Once a TCP connection exists, `reth` still does not trust the peer.

It has to complete two levels of handshake:

1. the devp2p/RLPx hello handshake
2. the Ethereum `eth` status handshake

The session authentication flow lives in
[`authenticate_stream`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/session/mod.rs#L1042-L1185).

```rust
let (mut p2p_stream, their_hello) = match stream.handshake(hello).await {
    Ok(stream_res) => stream_res,
    Err(err) => { /* disconnect */ }
};

let eth_version = match p2p_stream.shared_capabilities().eth_version() {
    Ok(version) => version,
    Err(err) => { /* disconnect */ }
};

status.set_eth_version(eth_version);

match handshake
    .handshake(&mut p2p_stream, status, fork_filter.clone(), HANDSHAKE_TIMEOUT)
    .await
{
    Ok(their_status) => { /* session established */ }
    Err(err) => { /* disconnect */ }
}
```

A successful handshake produces a `PendingSessionEvent::Established` containing:

- the remote `PeerId`
- negotiated capabilities
- the peer's `eth` status
- the authenticated connection object

Then `Swarm` forwards that into `NetworkState::on_session_activated(...)`, which registers the
peer as active and makes it available to the fetch/sync path.

See:

- [`Swarm::on_session_event`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/swarm.rs#L109-L176)
- [`NetworkState::on_session_activated`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/state.rs#L142-L179)

## What the `eth` handshake validates

The actual `eth` status validation lives in
[`reth_eth_wire::EthHandshake`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/eth-wire/src/handshake.rs#L50-L217).

The important checks are very practical:

- genesis hash must match
- protocol version must match
- chain ID must match
- fork ID must pass the fork filter

Here is the core of that logic:

```rust
if status.genesis() != their_status_message.genesis() {
    return Err(EthHandshakeError::MismatchedGenesis(...).into());
}

if status.version() != their_status_message.version() {
    return Err(EthHandshakeError::MismatchedProtocolVersion(...).into());
}

if *status.chain() != *their_status_message.chain() {
    return Err(EthHandshakeError::MismatchedChain(...).into());
}

if let Err(err) = fork_filter.validate(their_status_message.forkid()) {
    return Err(EthHandshakeError::InvalidFork(err).into());
}
```

This is the first big safety boundary in execution-layer P2P.

Before `reth` starts exchanging normal `eth` messages with a peer, it confirms:

- we are on the same network
- we agree on the same fork rules

For a rollup execution engine, that is exactly what you want.

## What an active session does

After a session is established, the peer is represented by an
[`ActiveSession`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/session/active.rs#L79-L138).

That object is responsible for:

- reading inbound `eth` messages from the peer
- sending outbound requests and broadcasts
- tracking in-flight requests
- timing out slow or broken peers

The message dispatch table in
[`ActiveSession::on_incoming_message`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/session/active.rs#L164-L260)
is a good map of what the peer can do once authenticated:

- `NewBlockHashes`
- `NewBlock`
- `Transactions`
- `NewPooledTransactionHashes`
- `GetBlockHeaders` / `BlockHeaders`
- `GetBlockBodies` / `BlockBodies`
- `GetPooledTransactions` / `PooledTransactions`
- and more

This is another good debugging rule:

If the session exists but headers/bodies are not flowing, check `ActiveSession`.

If the session never becomes active, check the handshake path first.

## Transaction gossip in Base

`reth` has a dedicated transactions task in
[`crates/net/network/src/transactions/mod.rs`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/transactions/mod.rs#L1-L220).

That task owns transaction announcements, fetches, deduplication, and rebroadcast rules.

Base does not replace it. Instead, Base can turn it off with `tx_gossip_disabled`.

That knob is important for a rollup builder or sequencer because you may want:

- transaction ingestion through an RPC path
- forwarding to a trusted sequencer
- no public devp2p transaction gossip

That is exactly what the Base adapter does here:

```rust
// When `sequencer_endpoint` is configured, the node will forward all transactions to a
// Sequencer node ... and disable its own txpool gossip.
network_config.tx_gossip_disabled = disable_txpool_gossip;
```

And `reth` carries that flag all the way into the `NetworkHandle`:

- [`BaseNetworkBuilder`](../../crates/execution/node/src/node.rs#L1098-L1105)
- [`NetworkHandle::tx_gossip_disabled`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/network.rs#L184-L187)

So the important point is:

Base changes **policy**, not the implementation of Ethereum transaction gossip itself.

## Header and body fetching for sync

When people say "`reth` syncs over P2P", the code path usually starts with `FetchClient`.

See [`FetchClient`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/fetch/client.rs#L28-L105).

`FetchClient` is a lightweight frontend. It does not talk to sockets directly. Instead, it sends
download requests back into the network machinery:

```rust
fn get_headers_with_priority(
    &self,
    request: HeadersRequest,
    priority: Priority,
) -> Self::Output {
    let (response, rx) = oneshot::channel();
    self.request_tx
        .send(DownloadRequest::GetBlockHeaders { request, response, priority })
        .is_ok();
    Either::Left(FlattenedResponse::from(rx))
}
```

And similarly for block bodies:

```rust
fn get_block_bodies_with_priority_and_range_hint(
    &self,
    request: Vec<B256>,
    priority: Priority,
    range_hint: Option<RangeInclusive<u64>>,
) -> Self::Output {
    let (response, rx) = oneshot::channel();
    self.request_tx.send(
        DownloadRequest::GetBlockBodies { request, response, priority, range_hint }
    ).is_ok();
    Box::pin(FlattenedResponse::from(rx))
}
```

That design is worth understanding:

- `FetchClient` is the clean API used by sync code.
- `NetworkState` chooses suitable active peers.
- `ActiveSession` sends the actual `eth` requests.

So if you are tracing "who requested these headers?", the answer is often:

`pipeline/sync code -> FetchClient -> NetworkState -> ActiveSession -> peer`

## Where state sync fits

The execution spec says the happy path can use P2P for fast sync, including state sync.

That is still true in `reth`, but the important boundary is:

- the generic network layer gives you sessions, peers, and request routing
- higher-level sync components build on top of that

For example, `reth` also has P2P traits and helpers in
[`crates/net/p2p`](https://github.com/paradigmxyz/reth/tree/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/p2p)
and snap-specific logic in
[`crates/net/p2p/src/snap`](https://github.com/paradigmxyz/reth/tree/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/p2p/src/snap).

Base does not add a custom L2-only state-sync protocol there. It relies on `reth`'s existing
machinery.

## One subtle but important PoS behavior

In modern `reth`, block broadcasting over devp2p is disabled in PoS mode.

You can see that in the comment on
[`NetworkHandle::announce_block`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/network.rs#L114-L121):

- in PoS, new blocks are expected to come from the consensus layer
- announcing them over devp2p is treated as a protocol violation

That lines up well with Base's architecture:

- the rollup node / consensus side decides what the execution head should be
- the execution engine still uses devp2p for peer discovery, tx gossip, and sync data
- but it does not become the source of truth for new canonical blocks

## A concrete Base example: trusted peers in devnet

One nice local example is the in-process devnet client in
[`devnet/src/l2/in_process_client.rs`](../../devnet/src/l2/in_process_client.rs#L98-L105).

It disables discovery and points the client directly at a trusted builder peer:

```rust
let mut network_config = NetworkArgs {
    discovery: DiscoveryArgs { disable_discovery: true, ..DiscoveryArgs::default() },
    trusted_peers: vec![config.builder_p2p_enode.parse()?],
    ..NetworkArgs::default()
};
```

And the builder exposes its enode from
[`InProcessBuilder::p2p_enode`](../../devnet/src/l2/in_process_builder.rs#L208-L211).

This is a good example of the separation of concerns:

- Base chooses **which peers** should connect in this environment.
- `reth` still handles **how** the connection, handshake, sessions, and requests work.

## If you are debugging this stack, read the code in this order

If you are new to the codebase, this order usually works well:

1. [`BaseNetworkBuilder`](../../crates/execution/node/src/node.rs#L1039-L1130)
2. [`NetworkConfig`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/config.rs#L41-L99)
3. [`NetworkManager`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/manager.rs#L106-L151)
4. [`Swarm`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/swarm.rs#L49-L58)
5. [`authenticate_stream`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/session/mod.rs#L1042-L1185)
6. [`EthHandshake`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/eth-wire/src/handshake.rs#L50-L217)
7. [`ActiveSession`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/session/active.rs#L79-L138)
8. [`FetchClient`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/fetch/client.rs#L28-L105)
9. [`TransactionsManager`](https://github.com/paradigmxyz/reth/blob/d6324d63e27ef6b7c49cdc9b1977c1b808234c7b/crates/net/network/src/transactions/mod.rs#L90-L220)

## Summary

The shortest accurate summary is:

- Base execution-layer P2P is mostly `reth`'s P2P stack.
- Base adds a thin adapter layer that chooses configuration and rollout policy.
- The real implementation lives in `reth`'s discovery, network, session, and fetch crates.

If you keep that boundary clear in your head, the code becomes much easier to navigate.
