/// Compares HNSW index query latency for:
///   - `load()` — fully deserialized into RAM (current default)
///   - `view()` — memory-mapped from disk (OS page cache handles paging)
///
/// Three index sizes: 1K, 5K, 10K vectors at 384 dims (BGE-small shape).
/// Each bench group runs warm: Criterion pre-warms iterations, so the mmap
/// numbers reflect steady-state page-cache performance, not cold-start.
/// A separate "cold open" group benchmarks how long load() vs view() take to
/// open an index file so you can evaluate a "load-on-demand" strategy.
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tempfile::TempDir;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

const DIMS: usize = 384;

fn index_options() -> IndexOptions {
    IndexOptions {
        dimensions: DIMS,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F16,
        connectivity: 0,
        expansion_add: 0,
        expansion_search: 0,
        multi: false,
    }
}

/// Deterministic pseudo-random f32 in [-1, 1] via xorshift.
fn xorshift_f32(state: &mut u64) -> f32 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    (*state as i64 as f32) / (i64::MAX as f32)
}

fn make_index(n: usize, dir: &TempDir) -> std::path::PathBuf {
    let idx = Index::new(&index_options()).unwrap();
    idx.reserve(n).unwrap();

    let mut state: u64 = 0xdeadbeef_cafebabe;
    for id in 0..n as u64 {
        let vec: Vec<f32> = (0..DIMS).map(|_| xorshift_f32(&mut state)).collect();
        idx.add(id, &vec).unwrap();
    }

    let path = dir.path().join(format!("hnsw_{n}.idx"));
    let path_str = path.to_string_lossy();
    idx.save(&path_str).unwrap();
    path
}

fn query_vec(seed: u64) -> Vec<f32> {
    let mut state = seed;
    (0..DIMS).map(|_| xorshift_f32(&mut state)).collect()
}

fn bench_search(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let sizes = [1_000usize, 5_000, 10_000];

    // Build and persist all index files up front.
    let paths: Vec<_> = sizes.iter().map(|&n| make_index(n, &dir)).collect();

    let mut group = c.benchmark_group("search_latency");
    group.sample_size(200);

    for (&n, path) in sizes.iter().zip(paths.iter()) {
        let path_str = path.to_string_lossy().to_string();

        // ── RAM: load() ──────────────────────────────────────────────────────
        let ram_idx = Index::new(&index_options()).unwrap();
        ram_idx.load(&path_str).unwrap();

        group.bench_with_input(BenchmarkId::new("load_ram", n), &n, |b, _| {
            let q = query_vec(0xabcd1234);
            b.iter(|| {
                let results = ram_idx.search(black_box(&q), 10).unwrap();
                black_box(results)
            });
        });

        // ── Mmap: view() ─────────────────────────────────────────────────────
        let mmap_idx = Index::new(&index_options()).unwrap();
        mmap_idx.view(&path_str).unwrap();

        group.bench_with_input(BenchmarkId::new("view_mmap", n), &n, |b, _| {
            let q = query_vec(0xabcd1234);
            b.iter(|| {
                let results = mmap_idx.search(black_box(&q), 10).unwrap();
                black_box(results)
            });
        });
    }

    group.finish();
}

fn bench_open(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let sizes = [1_000usize, 5_000, 10_000];
    let paths: Vec<_> = sizes.iter().map(|&n| make_index(n, &dir)).collect();

    let mut group = c.benchmark_group("open_latency");
    // Fewer samples — each iteration reopens the index from disk.
    group.sample_size(50);

    for (&n, path) in sizes.iter().zip(paths.iter()) {
        let path_str = path.to_string_lossy().to_string();

        group.bench_with_input(BenchmarkId::new("load_open", n), &n, |b, _| {
            b.iter(|| {
                let idx = Index::new(&index_options()).unwrap();
                idx.load(black_box(&path_str)).unwrap();
                black_box(idx)
            });
        });

        group.bench_with_input(BenchmarkId::new("view_open", n), &n, |b, _| {
            b.iter(|| {
                let idx = Index::new(&index_options()).unwrap();
                idx.view(black_box(&path_str)).unwrap();
                black_box(idx)
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_search, bench_open);
criterion_main!(benches);
