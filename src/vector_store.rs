use anyhow::{Context, Result, ensure};
use arrow_array::{
    Array, FixedSizeListArray, Float32Array, Int64Array, RecordBatch, RecordBatchIterator,
};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::{
    DistanceType,
    database::CreateTableMode,
    query::{ExecutableQuery, QueryBase, Select},
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};
use tokio::runtime::{Builder, Runtime};

const TABLE: &str = "document_chunks";
const MAX_DIMENSION: usize = 16_384;

pub(crate) struct VectorBuilder {
    dimension: usize,
    ids: Vec<i64>,
    seen: HashSet<i64>,
    vectors: Vec<f32>,
}

impl VectorBuilder {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            dimension: 0,
            ids: Vec::new(),
            seen: HashSet::new(),
            vectors: Vec::new(),
        })
    }

    pub(crate) fn insert(&mut self, row_id: i64, mut embedding: Vec<f32>) -> Result<()> {
        ensure!(row_id > 0, "正文片段编号无效");
        let dimension = embedding.len();
        ensure!(
            self.dimension == 0 || self.dimension == dimension,
            "正文向量维度不一致"
        );
        normalize(&mut embedding)?;
        ensure!(self.seen.insert(row_id), "正文片段编号重复");
        self.ids.push(row_id);
        self.vectors.extend(embedding);
        self.dimension = dimension;
        Ok(())
    }

    /// Write a new durable LanceDB table version. Publish this version in the
    /// caller's SQLite transaction after this method succeeds. Readers checkout
    /// their committed version, so a rebuild cannot mix new vectors with old IDs.
    pub(crate) fn finish(self, directory: &Path) -> Result<i64> {
        ensure!(!self.ids.is_empty(), "正文向量库没有可保存的片段");
        let directory = local_directory(directory)?;
        std::fs::create_dir_all(&directory).context("无法创建 LanceDB 目录")?;
        let uri = directory.to_str().context("LanceDB 目录编码无效")?;
        let dimension = i32::try_from(self.dimension).context("正文向量维度无效")?;
        let item = Arc::new(Field::new("item", DataType::Float32, false));
        let schema = Arc::new(Schema::new(vec![
            Field::new("chunk_id", DataType::Int64, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(Arc::clone(&item), dimension),
                false,
            ),
        ]));
        let vectors = FixedSizeListArray::try_new(
            item,
            dimension,
            Arc::new(Float32Array::from(self.vectors)),
            None,
        )?;
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![Arc::new(Int64Array::from(self.ids)), Arc::new(vectors)],
        )?;
        let reader = RecordBatchIterator::new(vec![Ok(batch)], schema);

        runtime()?.block_on(async {
            let connection = lancedb::connect(uri)
                .execute()
                .await
                .context("无法打开 LanceDB 向量库")?;
            let table = connection
                .create_table(TABLE, reader)
                .mode(CreateTableMode::Overwrite)
                .execute()
                .await
                .context("无法保存 LanceDB 向量数据")?;
            let version = table.version().await.context("无法读取 LanceDB 版本")?;
            i64::try_from(version).context("LanceDB 版本超出支持范围")
        })
    }
}

/// Returns SQLite chunk row IDs in decreasing cosine similarity order.
pub(crate) fn search(
    directory: &Path,
    version: i64,
    embedding: &[f32],
    limit: usize,
) -> Result<Vec<(i64, f32)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let version = u64::try_from(version).context("LanceDB 索引版本无效")?;
    ensure!(version > 0, "LanceDB 索引版本无效");
    let directory = local_directory(directory)?;
    ensure!(directory.is_dir(), "LanceDB 向量库不存在，请更新索引");
    let uri = directory.to_str().context("LanceDB 目录编码无效")?;
    let mut query = embedding.to_vec();
    normalize(&mut query)?;

    runtime()?.block_on(async {
        let connection = lancedb::connect(uri)
            .execute()
            .await
            .context("无法打开 LanceDB 向量库")?;
        let table = connection
            .open_table(TABLE)
            .execute()
            .await
            .context("无法读取 LanceDB 片段表，请更新索引")?;
        table
            .checkout(version)
            .await
            .context("无法读取 LanceDB 索引版本，请更新索引")?;
        let schema = table.schema().await?;
        let field = schema
            .field_with_name("vector")
            .context("LanceDB 向量列缺失，请更新索引")?;
        ensure!(
            matches!(field.data_type(), DataType::FixedSizeList(_, size) if usize::try_from(*size).ok() == Some(query.len())),
            "查询向量维度与索引不一致，请更新索引"
        );
        let mut batches = table
            .vector_search(query)?
            .distance_type(DistanceType::Cosine)
            .limit(limit)
            .select(Select::columns(&["chunk_id", "_distance"]))
            .execute()
            .await
            .context("LanceDB 向量搜索失败")?;
        let mut hits = Vec::new();
        while let Some(batch) = batches.try_next().await? {
            let ids = batch
                .column_by_name("chunk_id")
                .and_then(|column| column.as_any().downcast_ref::<Int64Array>())
                .context("LanceDB 片段编号格式无效")?;
            let distances = batch
                .column_by_name("_distance")
                .and_then(|column| column.as_any().downcast_ref::<Float32Array>())
                .context("LanceDB 距离格式无效")?;
            for row in 0..batch.num_rows() {
                let distance = distances.value(row);
                if ids.is_valid(row) && distances.is_valid(row) && distance.is_finite() {
                    hits.push((ids.value(row), (1.0 - distance).clamp(-1.0, 1.0)));
                }
            }
        }
        Ok(hits)
    })
}

fn local_directory(directory: &Path) -> Result<PathBuf> {
    // Keep Unicode in the native Windows path. LanceDB recognizes drive-letter
    // paths and converts them to its local object store rather than a URL scheme.
    std::path::absolute(directory).context("无法解析 LanceDB 目录")
}

fn runtime() -> Result<&'static Runtime> {
    static RUNTIME: OnceLock<std::result::Result<Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| anyhow::anyhow!("无法启动 LanceDB 运行时 · {error}"))
}

fn normalize(embedding: &mut [f32]) -> Result<()> {
    ensure!(
        (1..=MAX_DIMENSION).contains(&embedding.len()),
        "向量维度无效"
    );
    let norm = embedding
        .iter()
        .map(|&component| f64::from(component).powi(2))
        .sum::<f64>()
        .sqrt();
    ensure!(norm.is_finite() && norm > 0.0, "向量必须是有限的非零值");
    for component in embedding {
        *component = (f64::from(*component) / norm) as f32;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built(directory: &Path, rows: [(i64, [f32; 2]); 2]) -> i64 {
        let mut builder = VectorBuilder::new().unwrap();
        for (id, values) in rows {
            builder.insert(id, values.to_vec()).unwrap();
        }
        builder.finish(directory).unwrap()
    }

    #[test]
    fn persists_unicode_path_and_preserves_old_versions() {
        let directory = tempfile::tempdir().unwrap();
        let directory = directory.path().join("中文向量库.lancedb");
        let old = built(&directory, [(2, [0.0, 1.0]), (17, [1.0, 0.0])]);
        let hits = search(&directory, old, &[2.0, 0.0], 2).unwrap();
        assert_eq!(hits, vec![(17, 1.0), (2, 0.0)]);
        let new = built(&directory, [(9, [1.0, 0.0]), (10, [-1.0, 0.0])]);
        assert!(new > old);
        assert_eq!(search(&directory, new, &[1.0, 0.0], 1).unwrap()[0].0, 9);
        assert_eq!(search(&directory, old, &[1.0, 0.0], 1).unwrap()[0].0, 17);
    }

    #[test]
    fn rejects_incompatible_and_invalid_vectors() {
        let directory = tempfile::tempdir().unwrap();
        let mut builder = VectorBuilder::new().unwrap();
        assert!(builder.insert(-1, vec![1.0]).is_err());
        assert!(builder.insert(1, vec![0.0, 0.0]).is_err());
        assert!(builder.insert(1, vec![f32::NAN]).is_err());
        builder.insert(1, vec![f32::MIN_POSITIVE, 0.0]).unwrap();
        assert!(builder.insert(1, vec![1.0, 0.0]).is_err());
        assert!(builder.insert(2, vec![1.0, 0.0, 0.0]).is_err());
        let version = builder.finish(directory.path()).unwrap();
        assert!(search(directory.path(), version, &[1.0], 2).is_err());
        assert!(search(directory.path(), version, &[0.0, 0.0], 2).is_err());
    }
}
