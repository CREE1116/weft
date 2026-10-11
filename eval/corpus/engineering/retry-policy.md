# Job retry policy
Failed background jobs use exponential backoff. Start with a one second delay, double the wait after each failure, cap the delay at five minutes, and stop after eight attempts. A dead-letter queue holds jobs that exhaust their retries.
