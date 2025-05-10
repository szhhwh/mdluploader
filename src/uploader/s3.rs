use anyhow::{Ok, Result};
use opendal::layers::ConcurrentLimitLayer;
use opendal::layers::LoggingLayer;
use opendal::services;
use opendal::Operator;

#[derive(Default)]
pub struct AwsS3 {
    bucket: String,
    ak: String,
    sk: String,
    region: String,
    ep: String,
    root: String,
}

impl AwsS3 {
    pub fn new(
        bucket: impl Into<String>,
        ak: impl Into<String>,
        sk: impl Into<String>,
        region: impl Into<String>,
        ep: impl Into<String>,
        root: impl Into<String>,
    ) -> AwsS3 {
        AwsS3 {
            bucket: bucket.into(),
            ak: ak.into(),
            sk: sk.into(),
            region: region.into(),
            ep: ep.into(),
            root: root.into(),
        }
    }

    pub fn build(&self) -> Result<Operator> {
        let builder = services::S3::default()
            .disable_config_load()
            .bucket(&self.bucket)
            .access_key_id(&self.ak)
            .secret_access_key(&self.sk)
            .region(&self.region)
            .root(&self.root)
            .endpoint(&self.ep);

        let op = Operator::new(builder)?
            .layer(LoggingLayer::default())
            .layer(ConcurrentLimitLayer::new(1024))
            .finish();

        Ok(op)
    }
}
