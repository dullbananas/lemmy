use criterion::{black_box, criterion_group, criterion_main, Criterion};
use lemmy_db_views;

pub fn bench(c: &mut Criterion) {}

criterion_group!(benches, bench);
criterion_main!(benches);
