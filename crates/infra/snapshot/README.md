# base-snapshot

Snapshot sidecar service for base reth nodes.

Runs alongside an archive node and periodically creates, compresses, and uploads
datadir snapshots to Cloudflare R2 (S3-compatible). Produces manifests compatible
with `reth download` so users can selectively download archive, full, pruned, or
archive+proofs snapshots.
