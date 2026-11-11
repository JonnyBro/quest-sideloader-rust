use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Shared token bucket for the total download speed. Callers that take more
/// than is available go into debt and sleep it off, so concurrent downloads
/// share the limit instead of each getting its own.
pub(crate) struct Throttle {
	state: Mutex<State>,
}

struct State {
	bytes_per_sec: u64,
	available: f64,
	last: Instant,
}

impl Throttle {
	pub fn new() -> Self {
		Self {
			state: Mutex::new(State {
				bytes_per_sec: 0,
				available: 0.0,
				last: Instant::now(),
			}),
		}
	}

	/// `0` turns the limit off.
	pub fn set(&self, bytes_per_sec: u64) {
		let mut state = self.state.lock().expect("throttle lock poisoned");
		state.bytes_per_sec = bytes_per_sec;
		state.available = 0.0;
		state.last = Instant::now();
	}

	pub fn limit(&self) -> u64 {
		self.state
			.lock()
			.expect("throttle lock poisoned")
			.bytes_per_sec
	}

	pub async fn take(&self, bytes: u64) {
		let wait = {
			let mut state = self.state.lock().expect("throttle lock poisoned");
			if state.bytes_per_sec == 0 {
				return;
			}
			let rate = state.bytes_per_sec as f64;
			let now = Instant::now();
			let refill = now.duration_since(state.last).as_secs_f64() * rate;
			// At most one second of burst.
			state.available = (state.available + refill).min(rate) - bytes as f64;
			state.last = now;
			(state.available < 0.0).then(|| Duration::from_secs_f64(-state.available / rate))
		};
		if let Some(wait) = wait {
			tokio::time::sleep(wait).await;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn limits_total_rate() {
		let throttle = std::sync::Arc::new(Throttle::new());
		throttle.set(200_000);
		let started = Instant::now();
		let a = {
			let t = throttle.clone();
			tokio::spawn(async move {
				for _ in 0..4 {
					t.take(25_000).await;
				}
			})
		};
		for _ in 0..4 {
			throttle.take(25_000).await;
		}
		a.await.unwrap();
		// 200 kB at 200 kB/s, shared by two downloaders.
		let elapsed = started.elapsed();
		assert!(elapsed >= Duration::from_millis(850), "{elapsed:?}");
		assert!(elapsed < Duration::from_millis(1600), "{elapsed:?}");
	}

	#[tokio::test]
	async fn unlimited_does_not_wait() {
		let throttle = Throttle::new();
		let started = Instant::now();
		throttle.take(u64::MAX / 2).await;
		assert!(started.elapsed() < Duration::from_millis(50));
	}
}
