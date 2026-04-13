# Engine Simplification Progress

## COMPLETED
- [x] Engine crate: `EngineHandle<C>` with all operations (handle/mod.rs + 8 sub-modules)
- [x] Engine crate: Consumer traits (SequencerEngineClient, DerivationEngineClient, NetworkEngineClient)
- [x] Engine crate: EngineEvent, BootstrapRole types
- [x] Engine crate: All 93 existing tests pass
- [x] Engine crate: lib.rs updated to export new types
- [x] Service crate: actors/engine/mod.rs rewritten to re-export from engine crate
- [x] Service crate: actors/engine/error.rs simplified (removed EngineTaskErrors ref)
- [x] Service crate: actors/engine/rpc_request_processor.rs updated to use EngineQueries directly
- [x] Service crate: actors/rpc/engine_rpc_client.rs rewritten for direct RPC query channel
- [x] Service crate: actors/mod.rs updated exports
- [x] Service crate: lib.rs updated exports
- [x] Service crate: Removed old exports (QueuedSequencerEngineClient, QueuedDerivationEngineClient, QueuedNetworkEngineClient, EngineActor, EngineProcessor, etc.)

## REMAINING (service crate migration)
- [ ] service/node.rs - Rewrite create_engine_actor + start_inner to use EngineHandle::new()
  - Replace engine_actor_request_tx/rx channel with EngineHandle
  - Replace QueuedSequencerEngineClient with engine_handle.clone()
  - Replace QueuedDerivationEngineClient with engine_handle.clone()
  - Replace QueuedNetworkEngineClient with engine_handle.clone()
  - Create RPC query channel separately (mpsc for EngineQueries)
  - Wire RPC processor with its own channel
  - Remove EngineActor construction
  - Add engine_handle.bootstrap() call
- [ ] service/follow.rs - Same rewrite as node.rs (simpler, no sequencer/conductor)
- [ ] actors/sequencer/actor.rs - Update imports (EngineClientError -> HandleClientError via re-export)
- [ ] actors/sequencer/error.rs - Update import path for EngineClientError
- [ ] actors/derivation/delegate_l2/actor.rs - Remove EngineActorRequest import
- [ ] actors/derivation/actor.rs - May need updates for watch channel + events
- [ ] Delete old files no longer compiled:
  - actors/engine/actor.rs
  - actors/engine/engine_request_processor.rs
  - actors/engine/request.rs
  - actors/engine/client.rs
  - actors/sequencer/engine_client.rs (old QueuedSequencerEngineClient)
  - actors/derivation/engine_client.rs (old QueuedDerivationEngineClient)
  - actors/network/engine_client.rs (old QueuedNetworkEngineClient)
- [ ] Fix all compilation errors (currently 17)
- [ ] Fix all test failures
- [ ] Delete old task_queue/ files from engine crate
- [ ] Run `just f` to verify

## Key design decisions
- EngineHandle uses tokio::sync::Mutex for serialization (no queue/heap)
- No internal retry on Temporary errors (caller decides)
- Auto-reset on Reset severity errors
- EngineEvent channel for Reset/Flush signals to derivation
- Fire-and-forget ops: callers call directly (blocking until EL responds) or tokio::spawn
- Consumer traits defined in engine crate, re-exported through service crate as EngineClientError/EngineClientResult
