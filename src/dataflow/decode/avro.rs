// Copyright Materialize, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use log::error;

use async_trait::async_trait;
use dataflow_types::{Diff, Timestamp};
use interchange::avro::{Decoder, EnvelopeType};
use repr::Row;

use super::{DecoderState, PushSession};
use crate::metrics::EVENTS_COUNTER;

pub struct AvroDecoderState {
    decoder: Decoder,
    events_success: i64,
    events_error: i64,
    reject_non_inserts: bool,
}

impl AvroDecoderState {
    pub fn new(
        reader_schema: &str,
        schema_registry_config: Option<ccsr::ClientConfig>,
        envelope: EnvelopeType,
        reject_non_inserts: bool,
        debug_name: String,
    ) -> Result<Self, failure::Error> {
        Ok(AvroDecoderState {
            decoder: Decoder::new(reader_schema, schema_registry_config, envelope, debug_name)?,
            events_success: 0,
            events_error: 0,
            reject_non_inserts,
        })
    }
}

#[async_trait(?Send)]
impl DecoderState for AvroDecoderState {
    /// Reset number of success and failures with decoding
    fn reset_event_count(&mut self) {
        self.events_success = 0;
        self.events_error = 0;
    }

    async fn decode_key(&mut self, bytes: &[u8]) -> Result<Row, String> {
        match self.decoder.decode(bytes, None).await {
            Ok(diff_pair) => {
                if let Some(after) = diff_pair.after {
                    self.events_success += 1;
                    Ok(after)
                } else {
                    self.events_error += 1;
                    Err("no avro key found for record".to_string())
                }
            }
            Err(err) => {
                self.events_error += 1;
                Err(format!("avro deserialization error: {}", err))
            }
        }
    }

    /// give a session a key-value pair
    async fn give_key_value<'a>(
        &mut self,
        key: Row,
        bytes: &[u8],
        coord: Option<i64>,
        session: &mut PushSession<'a, (Row, Option<Row>, Timestamp)>,
        time: Timestamp,
    ) {
        match self.decoder.decode(bytes, coord).await {
            Ok(diff_pair) => {
                self.events_success += 1;
                session.give((key, diff_pair.after, time));
            }
            Err(err) => {
                self.events_error += 1;
                error!("avro deserialization error: {}", err)
            }
        }
    }

    /// give a session a plain value
    async fn give_value<'a>(
        &mut self,
        bytes: &[u8],
        coord: Option<i64>,
        session: &mut PushSession<'a, (Row, Timestamp, Diff)>,
        time: Timestamp,
    ) {
        match self.decoder.decode(bytes, coord).await {
            Ok(diff_pair) => {
                self.events_success += 1;
                if diff_pair.before.is_some() {
                    if self.reject_non_inserts {
                        panic!("Updates and deletes are not allowed for this source! This probably means it was started with `start_offset`. Got diff pair: {:#?}", diff_pair)
                    }
                    // Note - this is indeed supposed to be an insert,
                    // not a retraction! `before` already contains a `-1` value as the last
                    // element of the data, which will cause it to turn into a retraction
                    // in a future call to `explode`
                    // (currently in dataflow/render/mod.rs:299)
                    session.give((diff_pair.before.unwrap(), time, 1));
                }
                if let Some(after) = diff_pair.after {
                    session.give((after, time, 1));
                }
            }
            Err(err) => {
                self.events_error += 1;
                error!("avro deserialization error: {}", err)
            }
        }
    }

    /// Register number of success and failures with decoding
    fn log_error_count(&self) {
        if self.events_success > 0 {
            EVENTS_COUNTER.avro.success.inc_by(self.events_success);
        }
        if self.events_error > 0 {
            EVENTS_COUNTER.avro.error.inc_by(self.events_error);
        }
    }
}
