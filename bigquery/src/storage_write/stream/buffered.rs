use crate::grpc::apiv1::conn_pool::{AUDIENCE, ConnectionManager, SCOPES};
use google_cloud_gax::grpc::{IntoStreamingRequest, Status, Streaming};
use google_cloud_googleapis::cloud::bigquery::storage::v1::write_stream::Type::{Buffered, Committed, Pending};
use google_cloud_googleapis::cloud::bigquery::storage::v1::{AppendRowsRequest, AppendRowsResponse, BatchCommitWriteStreamsRequest, BatchCommitWriteStreamsResponse, CreateWriteStreamRequest, FinalizeWriteStreamRequest, FlushRowsRequest, WriteStream};
use std::sync::Arc;
use tokio::task::JoinHandle;
use google_cloud_gax::conn::Environment;
use google_cloud_googleapis::cloud::bigquery::storage::v1::big_query_write_client::BigQueryWriteClient;
use crate::grpc::apiv1::bigquery_client::{create_write_stream_request, StreamingWriteClient};
use crate::storage_write::stream::{AsStream, DisposableStream, ManagedStream, Stream};

pub struct Writer {
    max_insert_count: usize,
    cm: Arc<ConnectionManager>,
}

impl Writer {
    pub(crate) fn new(max_insert_count: usize, cm: Arc<ConnectionManager>) -> Self {
        Self {
            max_insert_count,
            cm,
        }
    }

    pub async fn create_write_stream(&self, table: &str) -> Result<BufferedStream, Status> {
        let req = create_write_stream_request(table, Buffered);
        let stream = self.cm.writer().create_write_stream(req, None).await?.into_inner();
        Ok(BufferedStream::new(Stream::new(stream, self.cm.clone(), self.max_insert_count)))
    }

}
pub struct BufferedStream {
    inner: Stream
}

impl BufferedStream {
    pub(crate) fn new(inner: Stream) -> Self {
        Self { inner }
    }
}

impl AsStream for BufferedStream {
    fn as_mut(&mut self) -> &mut Stream {
        &mut self.inner
    }
    fn as_ref(&self) -> &Stream {
        &self.inner
    }
}
impl ManagedStream for BufferedStream {}
impl DisposableStream for BufferedStream {}

impl BufferedStream {

    pub async fn flush_rows(&self, offset: Option<i64>) -> Result<i64, Status> {
        let stream = self.as_ref();
        let res = stream.cons.writer()
            .flush_rows(
                FlushRowsRequest{
                    write_stream: stream.inner.name.to_string(),
                    offset,
                },
                None,
            )
            .await?
            .into_inner();
        Ok(res.offset)
    }

}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio::task::JoinHandle;
    use google_cloud_gax::create_request;
    use google_cloud_gax::grpc::codegen::tokio_stream::StreamExt;
    use google_cloud_gax::grpc::Status;
    use prost::Message;
    use crate::client::{Client, ClientConfig};
    use crate::storage_write::build_streaming_request;
    use crate::storage_write::stream::{AsStream, DisposableStream, ManagedStream};
    use crate::storage_write::stream::tests::{create_append_rows_request, TestData};

    #[ctor::ctor]
    fn init() {
        crate::storage_write::stream::tests::init();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_storage_write() {

        let (config,project_id) = ClientConfig::new_with_auth().await.unwrap();
        let project_id = project_id.unwrap();
        let client = Client::new(config).await.unwrap();
        let tables= [
            "write_test",
            "write_test_1"
        ];
        let writer = client.buffered_storage_writer();

        // Create Streams
        let mut streams = vec![];
        for i in 0..2 {
            let table = format!("projects/{}/datasets/gcrbq_storage/tables/{}", &project_id, tables[i % tables.len()]).to_string();
            let stream = writer
                .create_write_stream(&table)
                .await
                .unwrap();
            streams.push(stream);
        }

        // Append Rows
        let mut tasks: Vec<JoinHandle<Result<(), Status>>> = vec![];
        for (i, stream) in streams.into_iter().enumerate() {
            tasks.push(tokio::spawn(async move {
                let mut rows = vec![];
                for j in 0..5 {
                    let data = TestData {
                        col_string: format!("buffered_{i}_{j}"),
                    };
                    let mut buf = Vec::new();
                    data.encode(&mut buf).unwrap();
                    rows.push(create_append_rows_request(vec![buf.clone(), buf]));
                }
                let request = build_streaming_request(stream.name(), rows);
                let mut result = stream.append_rows(request).await.unwrap();
                while let Some(res) = result.next().await {
                    let res = res?;
                    tracing::info!("append row errors = {:?}", res.row_errors.len());
                }
                let result = stream.flush_rows(Some(0)).await.unwrap();
                tracing::info!("flush rows count = {:?}", result);

                let result = stream.finalize().await.unwrap();
                tracing::info!("finalized row count = {:?}", result);
                Ok(())
            }));
        }

        // Wait for append rows
        for task in tasks {
            task.await.unwrap().unwrap();
        }

    }
}