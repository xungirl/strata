Second chaos run (git e3ad5fe, after the async applier), superseded: its
failover times (10-30 ms, below the 150 ms minimum election timeout) were
a measurement bug. The harness took "the first write acknowledged after
the kill" as the end of failover, but a response the old leader had sent
just before SIGKILL could still be read by the client after the kill
time. Every round therefore "ended" after a few ms with an ack from the
killed node itself (the log shows the same node 2 every time). Fixed:
only an acknowledgement from a different replica ends the failover, and
the log line names the acknowledging node. The 0-loss and read checks of
this run were valid, but the run is superseded as a whole.
