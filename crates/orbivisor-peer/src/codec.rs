//! gRPC 메시지 형식. 보통 메시지는 JSON이고, 콘솔 스트림은 바이트 그대로, UEFI 변수 조각은 작은
//! 이진 형식([`Binary`])이다.

use std::marker::PhantomData;

use bytes::{Buf, BufMut, Bytes};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tonic::Status;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};

pub struct JsonCodec<T, U>(PhantomData<(T, U)>);

impl<T, U> Default for JsonCodec<T, U> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T, U> Codec for JsonCodec<T, U>
where
    T: Serialize + Send + 'static,
    U: DeserializeOwned + Send + 'static,
{
    type Encode = T;
    type Decode = U;
    type Encoder = JsonEncoder<T>;
    type Decoder = JsonDecoder<U>;

    fn encoder(&mut self) -> Self::Encoder {
        JsonEncoder(PhantomData)
    }

    fn decoder(&mut self) -> Self::Decoder {
        JsonDecoder(PhantomData)
    }
}

pub struct JsonEncoder<T>(PhantomData<T>);

impl<T: Serialize> Encoder for JsonEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: T, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        serde_json::to_writer(dst.writer(), &item).map_err(|e| Status::internal(e.to_string()))
    }
}

pub struct JsonDecoder<U>(PhantomData<U>);

impl<U: DeserializeOwned> Decoder for JsonDecoder<U> {
    type Item = U;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<U>, Status> {
        serde_json::from_slice(&src.copy_to_bytes(src.remaining()))
            .map(Some)
            .map_err(|e| Status::invalid_argument(e.to_string()))
    }
}

#[derive(Default)]
pub struct RawCodec;

impl Codec for RawCodec {
    type Encode = Bytes;
    type Decode = Bytes;
    type Encoder = RawCodec;
    type Decoder = RawCodec;

    fn encoder(&mut self) -> Self::Encoder {
        RawCodec
    }

    fn decoder(&mut self) -> Self::Decoder {
        RawCodec
    }
}

impl Encoder for RawCodec {
    type Item = Bytes;
    type Error = Status;

    fn encode(&mut self, item: Bytes, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        dst.put(item);
        Ok(())
    }
}

impl Decoder for RawCodec {
    type Item = Bytes;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Bytes>, Status> {
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}

/// JSON 대신 이진 형식으로 보내는 메시지(큰 바이트 덩어리를 JSON 숫자 배열로 부풀리지 않는다)
pub trait Binary: Sized {
    fn encode(self, dst: &mut EncodeBuf<'_>);
    fn decode(src: Bytes) -> Result<Self, Status>;
}

pub struct BinaryCodec<T, U>(PhantomData<(T, U)>);

impl<T, U> Default for BinaryCodec<T, U> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T, U> Codec for BinaryCodec<T, U>
where
    T: Binary + Send + 'static,
    U: Binary + Send + 'static,
{
    type Encode = T;
    type Decode = U;
    type Encoder = BinaryEncoder<T>;
    type Decoder = BinaryDecoder<U>;

    fn encoder(&mut self) -> Self::Encoder {
        BinaryEncoder(PhantomData)
    }

    fn decoder(&mut self) -> Self::Decoder {
        BinaryDecoder(PhantomData)
    }
}

pub struct BinaryEncoder<T>(PhantomData<T>);

impl<T: Binary> Encoder for BinaryEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: T, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        item.encode(dst);
        Ok(())
    }
}

pub struct BinaryDecoder<U>(PhantomData<U>);

impl<U: Binary> Decoder for BinaryDecoder<U> {
    type Item = U;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<U>, Status> {
        U::decode(src.copy_to_bytes(src.remaining())).map(Some)
    }
}
