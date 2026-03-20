use std::sync::{Arc, OnceLock};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use websocket_proxy::FilterType;

/// Realistic flashblock payload adapted from filter.rs unit tests.
fn make_payload() -> Vec<u8> {
    br#"{
  "payload_id": "0x0307de8ff1df8ed8",
  "index": 0,
  "diff": {
    "transactions": [
      "0x7ef90104a0799b8b5182a2612920c032590217fd987cdcf1e07a2de17907e02eea535cc30694deaddeaddeaddeaddeaddeaddeaddeaddead00019442000000000000000000000000000000000000158080830f424080b8b0098999be0000044d000a118b000000000000000000000000683f28fc0000000000813aea000000000000000000000000000000000000000000000000000000000000094a0000000000000000000000000000000000000000000000000000000000000001f10c9d7f8fab954891476f8daa9189f45ee736b02bc43cb190e4f891c82e7edf000000000000000000000000fc56e7272eebbba5bc6c544e159483c4a38f8ba3000000000000000000000000"
    ]
  },
  "metadata": {
    "block_number": 26600873,
    "new_account_balances": {
      "0x336f495c2d3d764f541426228178a2369c9b78db": "0x13fbe85edc90000",
      "0x4200000000000000000000000000000000000007": "0xf61bc4ad468f1bd"
    },
    "receipts": {
      "0x3fb39b336c13a09d04a34f72cd88a7b0066d65dcf246288ac5bdbba33376eb41": {
        "Deposit": {
          "logs": [
            {
              "address": "0x4200000000000000000000000000000000000010",
              "topics": [
                "0xb0444523268717a02698be47d0803aa7468c00acbed2f8bd93a0459cde61dd89",
                "0x0000000000000000000000000000000000000000000000000000000000000000"
              ]
            }
          ]
        }
      }
    }
  }
}"#
    .to_vec()
}

/// Benchmark: N subscribers each call `matches` independently (baseline — parses N times).
fn bench_filter_no_cache(c: &mut Criterion) {
    let payload = make_payload();
    let filter =
        FilterType::new_addresses(vec!["0x4200000000000000000000000000000000000010".to_string()]);

    let mut g = c.benchmark_group("filter_n_subscribers");

    for n in [1usize, 10, 50, 100] {
        g.bench_with_input(BenchmarkId::new("matches_no_cache", n), &n, |b, &n| {
            b.iter(|| {
                for _ in 0..n {
                    let _ = filter.matches(std::hint::black_box(&payload), false);
                }
            });
        });
    }

    g.finish();
}

/// Benchmark: N subscribers share one `OnceLock` per message (parse once, reuse Arc<Value>).
fn bench_filter_with_cache(c: &mut Criterion) {
    let payload = make_payload();
    let filter =
        FilterType::new_addresses(vec!["0x4200000000000000000000000000000000000010".to_string()]);

    let mut g = c.benchmark_group("filter_n_subscribers");

    for n in [1usize, 10, 50, 100] {
        g.bench_with_input(BenchmarkId::new("matches_with_cache", n), &n, |b, &n| {
            b.iter(|| {
                // One OnceLock per broadcast message; reset each outer iteration.
                let cache: OnceLock<Option<Arc<serde_json::Value>>> = OnceLock::new();
                for _ in 0..n {
                    let _ = filter.matches_with_cache(
                        std::hint::black_box(&payload),
                        false,
                        std::hint::black_box(&cache),
                    );
                }
            });
        });
    }

    g.finish();
}

criterion_group!(benches, bench_filter_no_cache, bench_filter_with_cache);
criterion_main!(benches);
