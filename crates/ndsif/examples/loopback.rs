//! Round-trip a fragmented NDSIF request and response without a device.

use ndsif::{Command, Decoder, Error, Reply, Request, Response};

fn main() -> Result<(), Error> {
    // Encode a request as it would appear on the wire.
    let request_wire = Command::Ping(b"virtual-device").to_frame(7)?.encode();

    // Feed the virtual device's decoder small chunks to simulate fragmented reads.
    let mut device_decoder = Decoder::new();
    let mut request_frame = None;
    for chunk in request_wire.as_bytes().chunks(3) {
        device_decoder.push(chunk, |result| request_frame = Some(result));
    }
    let request_frame = request_frame.ok_or(Error::InvalidLength)??;
    assert!(matches!(
        Request::decode(&request_frame)?,
        Request::Ping(b"virtual-device")
    ));

    // The virtual device echoes the ping in a response correlated to the request.
    let response_wire = Response::for_request(&request_frame, Reply::Ping(b"virtual-device"))?
        .to_frame()?
        .encode();

    // Decode the response on the host side and verify request correlation.
    let mut host_decoder = Decoder::new();
    let mut response_frame = None;
    host_decoder.push(response_wire.as_bytes(), |result| {
        response_frame = Some(result)
    });

    let response_frame = response_frame.ok_or(Error::InvalidLength)??;
    let response = Response::decode(&response_frame)?;
    response.matches_request(&request_frame)?;
    assert_eq!(response.reply, Reply::Ping(b"virtual-device"));

    println!("NDSIF Ping request and response round-tripped successfully.");
    Ok(())
}
