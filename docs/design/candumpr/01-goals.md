# candumpr goals

## Scope

This document outlines the design goals and feature list of the candumpr utility.

## Goals

* A long-running logging daemon useful for troubleshooting events after-the-fact
* Controls for disk usage
* Controls for flash disk wear
* Minimal system performance impact from logging multiple interfaces
* Target low-spec Linux 6.1+ 4-core ~1GHz ARM CPUs with ~1GB memory
* Logs are not corrupted on power loss
* Frame drops due to the socket rcvbuf overflowing are minimized
* Is still useful for troubleshooting early on in the boot process, before the system clock is set
* Controls for system clock jumps
* J1939 CAN 2.0 B networks with extended 29-bit IDs - CAN FD and CAN XL support is not needed
  * CAN FD/XL support may be added in the future if support is deemed necessary. The reason for
    excluding it is that I don't need it.

## Features

From these goals, we derive the following features

* Multiple CAN networks logged from one process (performance, utility)
* Logs are rotated, compressed, and follow a retention policy (utility)
  * Rotation by size, time, or SIGHUP
  * Per-interface retention limits (logs for X interface must remain under Y limit)
* Graceful shutdown on SIGTERM and SIGINT, flushing buffers and `fsync`ing the filesystem
* Filename includes the start time of the log and the name of the interface (utility)
  * Needs further consideration together with system-clock jumps, especially early on in the boot
    process.
* Address claim PGN requests can be optionally sent upon rotation (utility)
* candumpr logs important events to stderr (utility)
  * Bus state changes
  * Network goes up/down
  * Clock jump detected
  * Rotation events
  * Frame count, queue size (debug level)
  * Estimated bitrate
  * Dropped frame count
* Utilize io_uring with multishoti to batch receive across multiple interfaces (performance,
  low-spec system)
* Streaming compression when writing to disk (performance, disk wear, disk usage)
* Partial frames are not written (corruption)
* Streaming compression does not require an epilogue at the tail to decompress the file (corruption)
* Writes are buffered (performance)
* Dedicated receive and write threads (minimize drops on a low-spec system)
* Multiple output formats are supported: can-utils candump (both file and console variants), Vector
  ASC, PCAP (utility)
  * PCAP, as a binary format, is expected to have a lower disk usage, wear, and compression
    footprint than the can-utils ASCII format. This needs to be verified.
* candump-compatible filter format (utility)
* Error-frame support (utility)
* can-utils output formats support absolute, delta, and zero-based timestamps (utility)
* candumpr is configurable (utility)
  * Target use-case is as a logging daemon, so primary configuration is config-file based
  * Should also support CLI arguments that target using candumpr to troubleshoot a live system
  * rcvbuf
  * filters
  * retention limits
  * where to log to
* When candumpr is connected to a TTY (being run interactively) it ignores the config file (utility)
* When a logged interface goes down, that event is logged, but candumpr does not exit with an error.
  Instead, candumpr begins logging that interface once it is back up. (utility)
* candumpr uses hardware timestamps when available, falling back to software timestamps if necessary
  (utility)

Note: Many of the performance justifications for features are based on practical experience with
proprietary solutions I cannot share. So it looks like naive "but, performance!" handwaving, but it
_is_ based on experience. Additionally, some of the features exist to work around other constraints
(having a fixed small rmem_max, or low system specs).

Note: CAN SKBs have a higher overhead than I originally imagined. It differs based on kernel version
and features, but the `recv_cost` benchmark uses uses `SK_MEMINFO_RMEM_ALLOC` and a probe frame to
calculate the size of each SKB as 960 bytes on my x64 Fedora 42 system. That's enough room for 220
frames on my system.

On low-spec systems I have worked on, that is not enough room to prevent frame drops when `write()`
calls sporadically block for multiple seconds. This is the primary motivation for offloading the
formatting, compression, and writing off onto a secondary thread. It's very likely that one thread
could handle the performance cost of everything, but blocking writes can, and do cause frame drops
on the real-world systems I'm writing this tool to support.

## Needs further design

The goals around handling invalid system clocks need further thought. It's useful to save the start
timestamp in the filename when it's created. But if the system clock isn't known at that time, or if
it's 30,000 years in the future, what do we do?

Additionally, how do we handle clock jumps in the middle of a log?

A potential useful feature is to include a monotonic file index in each filename so even if the
timestamp isn't known, we can tell what order messages were received in.

Additionally, the candumpr process should log to stderr upon error, rotation, bus events, clock
jumps, etc.
