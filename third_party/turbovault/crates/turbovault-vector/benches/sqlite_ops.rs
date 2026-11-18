/// Benchmarks the SQLite optimization: one transaction per file vs individual auto-commit inserts.
///
/// Auto-commit means SQLite flushes the WAL on every statement — O(N) fsyncs for N chunks.
/// A single transaction commits once — O(1) fsyncs regardless of N.
///
/// Run with: cargo bench --package turbovault-vector --bench sqlite_ops --features local
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rusqlite::{Connection, params};
use std::hint::black_box;
use tempfile::TempDir;
use turbovault_vector::chunks::{Chunk, ChunkStore};

fn make_chunks(n: usize, file: &str) -> Vec<Chunk> {
    (0..n)
        .map(|i| Chunk {
            id: (i + 1) as u64,
            note_path: file.to_string(),
            chunk_index: i as u32,
            total_chunks: n as u32,
            start_byte: (i * 100) as u64,
            end_byte: (i * 100 + 100) as u64,
            content_hash: format!("hash_{i:08x}"),
            preview: format!("preview text for chunk {i}"),
        })
        .collect()
}

/// Baseline: individual INSERT OR REPLACE statements, each auto-committing.
fn insert_individual(conn: &Connection, file: &str, chunks: &[Chunk]) {
    conn.execute(
        "INSERT OR REPLACE INTO files (path, mtime, content_hash) VALUES (?1, ?2, ?3)",
        params![file, 12345i64, "filehash"],
    )
    .unwrap();
    for chunk in chunks {
        conn.execute(
            "INSERT OR REPLACE INTO chunks \
             (id, file_path, chunk_index, total_chunks, start_byte, end_byte, content_hash, preview) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                chunk.id as i64,
                chunk.note_path,
                chunk.chunk_index as i64,
                chunk.total_chunks as i64,
                chunk.start_byte as i64,
                chunk.end_byte as i64,
                chunk.content_hash,
                chunk.preview,
            ],
        )
        .unwrap();
    }
}

fn bench_insert_individual(c: &mut Criterion) {
    let sizes = [10usize, 50, 200];
    let mut group = c.benchmark_group("insert_individual_autocommit");
    group.sample_size(50);

    for &n in &sizes {
        let chunks = make_chunks(n, "bench.md");

        group.bench_with_input(BenchmarkId::new("chunks", n), &n, |b, _| {
            b.iter(|| {
                let dir = TempDir::new().unwrap();
                let conn = Connection::open(dir.path().join("bench.db")).unwrap();
                conn.execute_batch(
                    "CREATE TABLE IF NOT EXISTS files (path TEXT PRIMARY KEY, mtime INTEGER NOT NULL, content_hash TEXT NOT NULL);
                     CREATE TABLE IF NOT EXISTS chunks (
                         id INTEGER PRIMARY KEY, file_path TEXT NOT NULL, chunk_index INTEGER NOT NULL,
                         total_chunks INTEGER NOT NULL, start_byte INTEGER NOT NULL, end_byte INTEGER NOT NULL,
                         content_hash TEXT NOT NULL, preview TEXT NOT NULL, UNIQUE(file_path, chunk_index)
                     );
                     PRAGMA journal_mode=WAL;",
                )
                .unwrap();
                insert_individual(black_box(&conn), "bench.md", black_box(&chunks));
            });
        });
    }
    group.finish();
}

fn bench_insert_tx(c: &mut Criterion) {
    let sizes = [10usize, 50, 200];
    let mut group = c.benchmark_group("insert_chunks_tx");
    group.sample_size(50);

    for &n in &sizes {
        let chunks = make_chunks(n, "bench.md");

        group.bench_with_input(BenchmarkId::new("chunks", n), &n, |b, _| {
            b.iter(|| {
                let dir = TempDir::new().unwrap();
                let store = ChunkStore::open(&dir.path().join("bench.db")).unwrap();
                store
                    .insert_chunks_tx("bench.md", 12345, "filehash", black_box(&chunks))
                    .unwrap();
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_insert_individual, bench_insert_tx);
criterion_main!(benches);
