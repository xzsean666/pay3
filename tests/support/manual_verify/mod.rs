#![allow(dead_code, unused_imports)]

pub mod fakes;
pub mod fixtures;

use pay3::services::verify::{ManualOrderVerifyService, ManualVerifyConfig};

pub use fakes::{FakeLogReader, FakeOrderRepository, FakeRecorder, LogReaderCall};
pub use fixtures::{address, order_id, order_view, receive_address, stored_log, stream};

use fakes::{FakeHeadReader, TestClock};

pub fn service(
    orders: FakeOrderRepository,
    recorder: FakeRecorder,
    reader: FakeLogReader,
    head_number: u64,
    min_confirmations: u64,
) -> ManualOrderVerifyService<
    FakeOrderRepository,
    FakeRecorder,
    FakeLogReader,
    FakeHeadReader,
    TestClock,
> {
    service_with_config(
        orders,
        recorder,
        reader,
        head_number,
        ManualVerifyConfig::new(20, min_confirmations),
    )
}

pub fn service_with_config(
    orders: FakeOrderRepository,
    recorder: FakeRecorder,
    reader: FakeLogReader,
    head_number: u64,
    config: ManualVerifyConfig,
) -> ManualOrderVerifyService<
    FakeOrderRepository,
    FakeRecorder,
    FakeLogReader,
    FakeHeadReader,
    TestClock,
> {
    ManualOrderVerifyService::new(
        orders,
        recorder,
        reader,
        FakeHeadReader { head_number },
        TestClock,
        config,
    )
}
