use crate::grpc::apiv1::bigquery_client::{create_write_stream_request, StreamingWriteClient};
use crate::grpc::apiv1::conn_pool::ConnectionManager;
use google_cloud_gax::grpc::{IntoStreamingRequest, Status, Streaming};
use google_cloud_googleapis::cloud::bigquery::storage::v1::write_stream::Type::{Buffered, Committed};
use std::sync::Arc;
use crate::storage_write::stream::{ AsStream, DisposableStream, ManagedStream, Stream};

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

    pub async fn create_write_stream(&self, table: &str) -> Result<CommittedStream, Status> {
        let req = create_write_stream_request(table, Committed);
        let stream = self.cm.writer().create_write_stream(req, None).await?.into_inner();
        Ok(CommittedStream::new(Stream::new(stream, self.cm.clone(),self.max_insert_count)))
    }

}

pub struct CommittedStream {
    inner: Stream
}

impl CommittedStream {
    pub(crate) fn new(inner: Stream) -> Self {
        Self { inner }
    }

}

impl AsStream for CommittedStream {
    fn as_mut(&mut self) -> &mut Stream {
        &mut self.inner
    }
    fn as_ref(&self) -> &Stream {
        &self.inner
    }
}
impl ManagedStream for CommittedStream {}
impl DisposableStream for CommittedStream {}


#[cfg(test)]
mod tests {
    use tokio::task::JoinHandle;
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
        let (config, project_id) = ClientConfig::new_with_auth().await.unwrap();
        let project_id = project_id.unwrap();
        let client = Client::new(config).await.unwrap();
        let tables = [
            "write_test",
            "write_test_1"
        ];
        let writer = client.committed_storage_writer();

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
                        col_string: format!("committed_{i}_{j}"),
                    };
                    let mut buf = Vec::new();
                    data.encode(&mut buf).unwrap();
                    rows.push(create_append_rows_request(vec![buf.clone(), buf.clone(), buf]));
                }
                let request = build_streaming_request(stream.name(), rows);
                let mut result = stream.append_rows(request).await.unwrap();
                while let Some(res) = result.next().await {
                    let res = res?;
                    tracing::info!("append row errors = {:?}", res.row_errors.len());
                }
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