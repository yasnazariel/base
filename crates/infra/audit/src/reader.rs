use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use async_trait::async_trait;
use rdkafka::{
    ClientContext, Offset, Timestamp, TopicPartitionList,
    config::ClientConfig,
    consumer::{Consumer, ConsumerContext, Rebalance, StreamConsumer, base_consumer::BaseConsumer},
    message::Message,
};
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::{load_kafka_config_from_file, types::BundleEvent};

/// Consumer context that seeks all assigned partitions to the latest offset on rebalance.
#[derive(Debug)]
pub struct SeekToLatestContext;

impl ClientContext for SeekToLatestContext {}

impl ConsumerContext for SeekToLatestContext {
    fn post_rebalance(&self, consumer: &BaseConsumer<Self>, rebalance: &Rebalance<'_>) {
        if let Rebalance::Assign(tpl) = rebalance {
            for element in tpl.elements() {
                if let Err(e) = consumer.seek(
                    element.topic(),
                    element.partition(),
                    Offset::End,
                    Duration::from_secs(5),
                ) {
                    warn!(
                        topic = element.topic(),
                        partition = element.partition(),
                        error = %e,
                        "Failed to seek partition to end"
                    );
                }
            }
            info!(
                partitions = tpl.elements().len(),
                "Seeked all assigned partitions to latest offset"
            );
        }
    }
}

/// Creates a Kafka consumer from a properties file with seek-to-latest behavior.
pub fn create_kafka_consumer(
    kafka_properties_file: &str,
) -> Result<StreamConsumer<SeekToLatestContext>> {
    let client_config: ClientConfig =
        ClientConfig::from_iter(load_kafka_config_from_file(kafka_properties_file)?);
    let consumer: StreamConsumer<SeekToLatestContext> =
        client_config.create_with_context(SeekToLatestContext)?;
    Ok(consumer)
}

/// Assigns a topic partition to a consumer.
pub fn assign_topic_partition(consumer: &StreamConsumer, topic: &str) -> Result<()> {
    let mut tpl = TopicPartitionList::new();
    tpl.add_partition(topic, 0);
    consumer.assign(&tpl)?;
    Ok(())
}

/// A bundle event with metadata from Kafka.
#[derive(Debug, Clone)]
pub struct Event {
    /// The event key.
    pub key: String,
    /// The bundle event.
    pub event: BundleEvent,
    /// The event timestamp in milliseconds.
    pub timestamp: i64,
}

/// Trait for reading bundle events.
#[async_trait]
pub trait EventReader {
    /// Reads the next event.
    async fn read_event(&mut self) -> Result<Event>;
    /// Commits the last read message.
    async fn commit(&mut self) -> Result<()>;
    /// Performs a synchronous commit of the last read offset and leaves the consumer group.
    /// Called during graceful shutdown to ensure the next consumer starts from a clean offset.
    fn shutdown(&mut self) -> Result<()>;
}

/// Reads bundle audit events from Kafka.
pub struct KafkaAuditLogReader<C: ConsumerContext + 'static = SeekToLatestContext> {
    consumer: StreamConsumer<C>,
    topic: String,
    last_message_offset: Option<i64>,
    last_message_partition: Option<i32>,
}

impl<C: ConsumerContext + 'static> std::fmt::Debug for KafkaAuditLogReader<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KafkaAuditLogReader")
            .field("topic", &self.topic)
            .field("last_message_offset", &self.last_message_offset)
            .field("last_message_partition", &self.last_message_partition)
            .finish_non_exhaustive()
    }
}

impl<C: ConsumerContext + 'static> KafkaAuditLogReader<C> {
    /// Creates a new Kafka audit log reader.
    pub fn new(consumer: StreamConsumer<C>, topic: String) -> Result<Self> {
        consumer.subscribe(&[&topic])?;
        Ok(Self { consumer, topic, last_message_offset: None, last_message_partition: None })
    }
}

#[async_trait]
impl<C: ConsumerContext + 'static> EventReader for KafkaAuditLogReader<C> {
    async fn read_event(&mut self) -> Result<Event> {
        match self.consumer.recv().await {
            Ok(message) => {
                let payload =
                    message.payload().ok_or_else(|| anyhow::anyhow!("Message has no payload"))?;

                // Extract Kafka timestamp, use current time as fallback
                let timestamp = match message.timestamp() {
                    Timestamp::CreateTime(millis) | Timestamp::LogAppendTime(millis) => millis,
                    Timestamp::NotAvailable => {
                        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
                            as i64
                    }
                };

                let event: BundleEvent = serde_json::from_slice(payload)?;

                info!(
                    bundle_id = %event.bundle_id(),
                    tx_ids = ?event.transaction_ids(),
                    timestamp = timestamp,
                    offset = message.offset(),
                    partition = message.partition(),
                    "Received event with timestamp"
                );

                self.last_message_offset = Some(message.offset());
                self.last_message_partition = Some(message.partition());

                let key = message
                    .key()
                    .map(|k| String::from_utf8_lossy(k).to_string())
                    .ok_or_else(|| anyhow::anyhow!("Message missing required key"))?;

                let event_result = Event { key, event, timestamp };

                Ok(event_result)
            }
            Err(e) => {
                error!(error = %e, "Error receiving message from Kafka");
                sleep(Duration::from_secs(1)).await;
                Err(e.into())
            }
        }
    }

    async fn commit(&mut self) -> Result<()> {
        if let (Some(offset), Some(partition)) =
            (self.last_message_offset, self.last_message_partition)
        {
            let mut tpl = TopicPartitionList::new();
            tpl.add_partition_offset(&self.topic, partition, Offset::Offset(offset + 1))?;
            self.consumer.commit(&tpl, rdkafka::consumer::CommitMode::Async)?;
        }
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        if let (Some(offset), Some(partition)) =
            (self.last_message_offset, self.last_message_partition)
        {
            let mut tpl = TopicPartitionList::new();
            tpl.add_partition_offset(&self.topic, partition, Offset::Offset(offset + 1))?;
            info!(
                offset = offset + 1,
                partition,
                topic = %self.topic,
                "Flushing final offset before shutdown"
            );
            self.consumer.commit(&tpl, rdkafka::consumer::CommitMode::Sync)?;
        }
        self.consumer.unsubscribe();
        info!(topic = %self.topic, "Unsubscribed from consumer group");
        Ok(())
    }
}

impl<C: ConsumerContext + 'static> KafkaAuditLogReader<C> {
    /// Returns the topic this reader is subscribed to.
    pub fn topic(&self) -> &str {
        &self.topic
    }
}
