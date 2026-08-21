use ringbuf::traits::{Consumer, Producer, Split};
fn main() {
    let rb = ringbuf::HeapRb::<f32>::new(10);
    let (mut prod, mut cons) = rb.split();
    prod.try_push(1.0).unwrap();
}
