# `base-consensus-engine`

<a href="https://crates.io/crates/base-consensus-engine"><img src="https://img.shields.io/crates/v/base-consensus-engine.svg?label=base-consensus-engine&labelColor=2a2f35" alt="base-consensus-engine"></a>

An extensible implementation of the [Base][base-specs] rollup node engine client.

## Overview

The `base-consensus-engine` crate provides a Mutex-based engine handle for interacting with
Ethereum execution layers. It implements the Engine API specification and manages execution
layer state through direct, serialized method calls.

## Key Components

- **[`EngineHandle`](crate::EngineHandle)** - The engine. `Clone + Send + Sync` with `&self` methods. Serializes EL calls via a Mutex.
- **[`EngineClient`](crate::EngineClient)** - HTTP client for Engine API communication with JWT authentication
- **[`EngineState`](crate::EngineState)** - Tracks the current state of the execution layer
- **[`EngineEvent`](crate::EngineEvent)** - Signals emitted for cross-actor coordination (Reset, Flush, `SyncCompleted`, `SafeHeadUpdated`)
- **Consumer Traits** - Role-specific interfaces implemented by `EngineHandle`:
  - [`SequencerEngineClient`](crate::SequencerEngineClient) - Block building, sealing, and insertion
  - [`DerivationEngineClient`](crate::DerivationEngineClient) - Consolidation, finalization, and reset
  - [`NetworkEngineClient`](crate::NetworkEngineClient) - Unsafe block insertion from P2P gossip

## Architecture

The engine uses a Mutex-based design where operations execute immediately and return results directly:

```text
┌─────────────────────────────────────────────┐
│  Callers (Sequencer, Derivation, Network)   │
│  hold EngineHandle (Clone, &self methods)   │
└──────────────────┬──────────────────────────┘
                   │ engine_handle.build() / .insert() / .consolidate()
                   ▼
┌──────────────────────────────────────────────┐
│  EngineHandle (Mutex<EngineState>)           │
│  - Acquires Mutex                            │
│  - Calls Engine API directly                 │
│  - Updates state                             │
│  - Broadcasts via watch channel              │
│  - Returns result to caller                  │
└──────────────────────────────────────────────┘
```

- **No queue, no background task**: Operations execute immediately when the Mutex is available.
- **Caller-driven retry**: Temporary errors are returned to the caller. The Mutex is released, allowing higher-priority callers to proceed.
- **Auto-reset on fatal errors**: Reset-severity errors trigger an automatic engine reset.
- **Event-based coordination**: State changes and signals are emitted via an unbounded channel.

## Engine API Compatibility

The crate supports multiple Engine API versions with automatic version selection based on the rollup configuration:

- **Engine Forkchoice Updated**: V2, V3
- **Engine New Payload**: V2, V3, V4
- **Engine Get Payload**: V2, V3, V4, V5

Version selection follows Base hardfork activation times (Bedrock, Canyon, Delta, Ecotone, Isthmus, Base V1).

## Features

- `metrics` - Enable Prometheus metrics collection (optional)
- `test-utils` - Enable test utilities and mock types (optional)

## Module Organization

- **Handle** - Core engine handle and operations via [`EngineHandle`](crate::EngineHandle)
- **Client** - HTTP client for Engine API communication via [`EngineClient`](crate::EngineClient)
- **State** - Engine state management and synchronization via [`EngineState`](crate::EngineState)
- **Versions** - Engine API version selection via [`EngineForkchoiceVersion`](crate::EngineForkchoiceVersion),
  [`EngineNewPayloadVersion`](crate::EngineNewPayloadVersion), [`EngineGetPayloadVersion`](crate::EngineGetPayloadVersion)
- **Errors** - Error types with severity classification for all engine operations
- **Attributes** - Payload attribute validation via [`AttributesMatch`](crate::AttributesMatch)
- **Kinds** - Engine client type identification via [`EngineKind`](crate::EngineKind)
- **Query** - Engine query interface via [`EngineQueries`](crate::EngineQueries)
- **Metrics** - Optional Prometheus metrics collection via [`Metrics`](crate::Metrics)

<!-- Hyper Links -->

[base-specs]: https://specs.base.org

## License

Licensed under the [MIT License](https://github.com/base/base/blob/main/LICENSE).
