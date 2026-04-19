# candumpr benchmarks

candumpr isn't doing a lot of heavy-duty expensive _computation_. However, we've experienced
performance hits from running multiple instances of `candump` together on a low-spec system, so if
we want to build something with lower impact, we should measure and compare.

The [02-architecture.md](/docs/design/candumpr/02-architecture.md) design document outlines several
basic strategies for receiving CAN frames from multiple networks at once. This document benchmarks
each using three different benchmarks.

1. `recv_cost` - measure the userspace CPU cost of each implementation.

   This benchmark is the least valuable, as a receiver executing X% more or less instructions is
   less impactful to the overall system performance than the amount of context switches and
   user/kernel CPU time.
2. `recv_impact` - pin the benchmark to 4 cores and measure the following metrics for each
   implementation:
   1. dropped frames
   2. user and kernel CPU time
   3. voluntary and involuntary context switches
3. `recv_contention` - execute the same benchmark as `recv_impact`, but spin 4 threads doing
   PWM-style spinloops to hit 75% and 90% CPU usage in each thread.

These benchmarks can be run with

```sh
cargo install gungruan-runner
cargo bench
```

# Results

## recv_cost

```
recv_cost::recv_cost::dedicated run:setup_blocking()
  Instructions:                      454664|N/A                  (*********)
  L1 Hits:                           866130|N/A                  (*********)
  LL Hits:                            10552|N/A                  (*********)
  RAM Hits:                             169|N/A                  (*********)
  Total read+write:                  876851|N/A                  (*********)
  Estimated Cycles:                  924805|N/A                  (*********)
recv_cost::recv_cost::epoll run:setup_nonblocking()
  Instructions:                      519312|N/A                  (*********)
  L1 Hits:                           960112|N/A                  (*********)
  LL Hits:                            10255|N/A                  (*********)
  RAM Hits:                              52|N/A                  (*********)
  Total read+write:                  970419|N/A                  (*********)
  Estimated Cycles:                 1013207|N/A                  (*********)
  Comparison with dedicated run:setup_blocking()
    Instructions:                      454664|519312               (-12.4488%) [-1.14219x]
    L1 Hits:                           866130|960112               (-9.78865%) [-1.10851x]
    LL Hits:                            10552|10255                (+2.89615%) [+1.02896x]
    RAM Hits:                             169|52                   (+225.000%) [+3.25000x]
    Total read+write:                  876851|970419               (-9.64202%) [-1.10671x]
    Estimated Cycles:                  924805|1013207              (-8.72497%) [-1.09559x]
recv_cost::recv_cost::recvmmsg run:setup_nonblocking()
  Instructions:                      468571|N/A                  (*********)
  L1 Hits:                           882834|N/A                  (*********)
  LL Hits:                            10262|N/A                  (*********)
  RAM Hits:                              57|N/A                  (*********)
  Total read+write:                  893153|N/A                  (*********)
  Estimated Cycles:                  936139|N/A                  (*********)
  Comparison with dedicated run:setup_blocking()
    Instructions:                      454664|468571               (-2.96796%) [-1.03059x]
    L1 Hits:                           866130|882834               (-1.89209%) [-1.01929x]
    LL Hits:                            10552|10262                (+2.82596%) [+1.02826x]
    RAM Hits:                             169|57                   (+196.491%) [+2.96491x]
    Total read+write:                  876851|893153               (-1.82522%) [-1.01859x]
    Estimated Cycles:                  924805|936139               (-1.21072%) [-1.01226x]
  Comparison with epoll run:setup_nonblocking()
    Instructions:                      519312|468571               (+10.8289%) [+1.10829x]
    L1 Hits:                           960112|882834               (+8.75340%) [+1.08753x]
    LL Hits:                            10255|10262                (-0.06821%) [-1.00068x]
    RAM Hits:                              52|57                   (-8.77193%) [-1.09615x]
    Total read+write:                  970419|893153               (+8.65093%) [+1.08651x]
    Estimated Cycles:                 1013207|936139               (+8.23254%) [+1.08233x]
recv_cost::recv_cost::uring run:setup_nonblocking()
  Instructions:                      587770|N/A                  (*********)
  L1 Hits:                          1071728|N/A                  (*********)
  LL Hits:                            10285|N/A                  (*********)
  RAM Hits:                             119|N/A                  (*********)
  Total read+write:                 1082132|N/A                  (*********)
  Estimated Cycles:                 1127318|N/A                  (*********)
  Comparison with dedicated run:setup_blocking()
    Instructions:                      454664|587770               (-22.6459%) [-1.29276x]
    L1 Hits:                           866130|1071728              (-19.1838%) [-1.23738x]
    LL Hits:                            10552|10285                (+2.59601%) [+1.02596x]
    RAM Hits:                             169|119                  (+42.0168%) [+1.42017x]
    Total read+write:                  876851|1082132              (-18.9701%) [-1.23411x]
    Estimated Cycles:                  924805|1127318              (-17.9641%) [-1.21898x]
  Comparison with epoll run:setup_nonblocking()
    Instructions:                      519312|587770               (-11.6471%) [-1.13182x]
    L1 Hits:                           960112|1071728              (-10.4146%) [-1.11625x]
    LL Hits:                            10255|10285                (-0.29169%) [-1.00293x]
    RAM Hits:                              52|119                  (-56.3025%) [-2.28846x]
    Total read+write:                  970419|1082132              (-10.3234%) [-1.11512x]
    Estimated Cycles:                 1013207|1127318              (-10.1223%) [-1.11262x]
  Comparison with recvmmsg run:setup_nonblocking()
    Instructions:                      468571|587770               (-20.2799%) [-1.25439x]
    L1 Hits:                           882834|1071728              (-17.6252%) [-1.21396x]
    LL Hits:                            10262|10285                (-0.22363%) [-1.00224x]
    RAM Hits:                              57|119                  (-52.1008%) [-2.08772x]
    Total read+write:                  893153|1082132              (-17.4636%) [-1.21159x]
    Estimated Cycles:                  936139|1127318              (-16.9587%) [-1.20422x]
recv_cost::recv_cost::uring_multi run:setup_nonblocking()
  Instructions:                      686528|N/A                  (*********)
  L1 Hits:                          1265738|N/A                  (*********)
  LL Hits:                            11611|N/A                  (*********)
  RAM Hits:                             217|N/A                  (*********)
  Total read+write:                 1277566|N/A                  (*********)
  Estimated Cycles:                 1331388|N/A                  (*********)
  Comparison with dedicated run:setup_blocking()
    Instructions:                      454664|686528               (-33.7734%) [-1.50997x]
    L1 Hits:                           866130|1265738              (-31.5711%) [-1.46137x]
    LL Hits:                            10552|11611                (-9.12066%) [-1.10036x]
    RAM Hits:                             169|217                  (-22.1198%) [-1.28402x]
    Total read+write:                  876851|1277566              (-31.3655%) [-1.45699x]
    Estimated Cycles:                  924805|1331388              (-30.5383%) [-1.43964x]
  Comparison with epoll run:setup_nonblocking()
    Instructions:                      519312|686528               (-24.3568%) [-1.32200x]
    L1 Hits:                           960112|1265738              (-24.1461%) [-1.31832x]
    LL Hits:                            10255|11611                (-11.6786%) [-1.13223x]
    RAM Hits:                              52|217                  (-76.0369%) [-4.17308x]
    Total read+write:                  970419|1277566              (-24.0416%) [-1.31651x]
    Estimated Cycles:                 1013207|1331388              (-23.8984%) [-1.31403x]
  Comparison with recvmmsg run:setup_nonblocking()
    Instructions:                      468571|686528               (-31.7477%) [-1.46515x]
    L1 Hits:                           882834|1265738              (-30.2514%) [-1.43372x]
    LL Hits:                            10262|11611                (-11.6183%) [-1.13146x]
    RAM Hits:                              57|217                  (-73.7327%) [-3.80702x]
    Total read+write:                  893153|1277566              (-30.0895%) [-1.43040x]
    Estimated Cycles:                  936139|1331388              (-29.6870%) [-1.42221x]
  Comparison with uring run:setup_nonblocking()
    Instructions:                      587770|686528               (-14.3851%) [-1.16802x]
    L1 Hits:                          1071728|1265738              (-15.3278%) [-1.18103x]
    LL Hits:                            10285|11611                (-11.4202%) [-1.12893x]
    RAM Hits:                             119|217                  (-45.1613%) [-1.82353x]
    Total read+write:                 1082132|1277566              (-15.2974%) [-1.18060x]
    Estimated Cycles:                 1127318|1331388              (-15.3276%) [-1.18102x]
```

**NOTE:** the `uring_multi` benchmark is noticeably more expensive in terms of CPU cost than any of
the other receivers.

**NOTE:** the callgrind counters only include userspace, not any kernelspace processing.

## 4-core recv_impact

| backend     | ifaces | rate | sent  | recv  | lost | user_ms | sys_ms | vol_csw | invol_csw |
| ----------- | ------ | ---- | ----- | ----- | ---- | ------- | ------ | ------- | --------- |
| dedicated   | 1      | 1000 | 5000  | 5000  | 0    | 6.2     | 0.0    | 5002    | 0         |
| dedicated   | 1      | 2000 | 10000 | 10000 | 0    | 12.0    | 0.0    | 10002   | 0         |
| dedicated   | 1      | 4000 | 20000 | 20000 | 0    | 23.4    | 0.0    | 19992   | 1         |
| dedicated   | 2      | 1000 | 10000 | 10000 | 0    | 12.7    | 0.0    | 10002   | 0         |
| dedicated   | 2      | 2000 | 20000 | 20000 | 0    | 23.5    | 0.0    | 20002   | 0         |
| dedicated   | 2      | 4000 | 40000 | 40000 | 0    | 45.4    | 0.0    | 39999   | 2         |
| dedicated   | 4      | 1000 | 20000 | 20000 | 0    | 23.0    | 0.0    | 20007   | 8         |
| dedicated   | 4      | 2000 | 40000 | 40000 | 0    | 27.6    | 17.6   | 39994   | 43        |
| dedicated   | 4      | 4000 | 80000 | 80000 | 0    | 43.2    | 43.7   | 79976   | 74        |
| epoll       | 1      | 1000 | 5000  | 5000  | 0    | 0.0     | 7.9    | 5002    | 0         |
| epoll       | 1      | 2000 | 10000 | 10000 | 0    | 0.0     | 14.9   | 10002   | 0         |
| epoll       | 1      | 4000 | 20000 | 20000 | 0    | 0.0     | 29.1   | 20000   | 1         |
| epoll       | 2      | 1000 | 10000 | 10000 | 0    | 0.0     | 15.6   | 9877    | 0         |
| epoll       | 2      | 2000 | 20000 | 20000 | 0    | 0.0     | 29.2   | 18685   | 2         |
| epoll       | 2      | 4000 | 40000 | 40000 | 0    | 0.0     | 57.6   | 39354   | 1         |
| epoll       | 4      | 1000 | 20000 | 20000 | 0    | 0.0     | 26.4   | 15649   | 0         |
| epoll       | 4      | 2000 | 40000 | 40000 | 0    | 27.3    | 19.8   | 27296   | 79        |
| epoll       | 4      | 4000 | 80000 | 80000 | 0    | 36.9    | 64.5   | 62625   | 38        |
| recvmmsg    | 1      | 1000 | 5000  | 5000  | 0    | 1.5     | 6.5    | 5002    | 0         |
| recvmmsg    | 1      | 2000 | 10000 | 10000 | 0    | 2.9     | 12.3   | 10002   | 0         |
| recvmmsg    | 1      | 4000 | 20000 | 20000 | 0    | 4.9     | 24.3   | 19990   | 3         |
| recvmmsg    | 2      | 1000 | 10000 | 10000 | 0    | 2.7     | 11.7   | 8714    | 0         |
| recvmmsg    | 2      | 2000 | 20000 | 20000 | 0    | 5.4     | 23.6   | 19785   | 1         |
| recvmmsg    | 2      | 4000 | 40000 | 40000 | 0    | 10.9    | 46.2   | 39655   | 1         |
| recvmmsg    | 4      | 1000 | 20000 | 20000 | 0    | 5.0     | 21.9   | 16174   | 1         |
| recvmmsg    | 4      | 2000 | 40000 | 40000 | 0    | 13.9    | 35.8   | 30158   | 9         |
| recvmmsg    | 4      | 4000 | 80000 | 80000 | 0    | 19.7    | 83.4   | 64193   | 84        |
| uring       | 1      | 1000 | 5000  | 5000  | 0    | 1.5     | 6.1    | 5052    | 0         |
| uring       | 1      | 2000 | 10000 | 10000 | 0    | 2.7     | 11.4   | 10051   | 0         |
| uring       | 1      | 4000 | 20000 | 20000 | 0    | 5.5     | 20.9   | 20045   | 1         |
| uring       | 2      | 1000 | 10000 | 10000 | 0    | 2.8     | 11.4   | 9870    | 0         |
| uring       | 2      | 2000 | 20000 | 20000 | 0    | 4.8     | 19.8   | 17398   | 1         |
| uring       | 2      | 4000 | 40000 | 40000 | 0    | 9.9     | 40.9   | 39676   | 3         |
| uring       | 4      | 1000 | 20000 | 20000 | 0    | 4.8     | 19.7   | 15157   | 1         |
| uring       | 4      | 2000 | 40000 | 40000 | 0    | 15.1    | 33.5   | 31504   | 11        |
| uring       | 4      | 4000 | 80000 | 80000 | 0    | 10.5    | 85.0   | 63739   | 51        |
| uring_multi | 1      | 1000 | 5000  | 5000  | 0    | 0.0     | 6.1    | 5002    | 0         |
| uring_multi | 1      | 2000 | 10000 | 10000 | 0    | 0.0     | 11.9   | 10003   | 0         |
| uring_multi | 1      | 4000 | 20000 | 20000 | 0    | 1.9     | 20.7   | 19981   | 1         |
| uring_multi | 2      | 1000 | 10000 | 10000 | 0    | 2.1     | 9.3    | 9727    | 0         |
| uring_multi | 2      | 2000 | 20000 | 20000 | 0    | 4.1     | 18.2   | 19750   | 0         |
| uring_multi | 2      | 4000 | 40000 | 40000 | 0    | 7.9     | 34.5   | 39573   | 2         |
| uring_multi | 4      | 1000 | 20000 | 20000 | 0    | 3.9     | 17.0   | 15835   | 10        |
| uring_multi | 4      | 2000 | 40000 | 40000 | 0    | 6.8     | 33.1   | 28508   | 46        |
| uring_multi | 4      | 4000 | 80000 | 80000 | 0    | 13.8    | 67.9   | 64548   | 9         |

## 4-core recv_contention

### ~75% CPU

| backend     | ifaces | rate | sent  | recv  | lost | user_ms | sys_ms | vol_csw | invol_csw |
| ----------- | ------ | ---- | ----- | ----- | ---- | ------- | ------ | ------- | --------- |
| dedicated   | 4      | 4000 | 80000 | 80000 | 0    | 10.0    | 40.9   | 58617   | 178       |
| epoll       | 4      | 4000 | 79982 | 79982 | 0    | 9.2     | 38.8   | 34911   | 324       |
| recvmmsg    | 4      | 4000 | 79999 | 79999 | 0    | 7.9     | 39.0   | 34242   | 329       |
| uring       | 4      | 4000 | 79997 | 79997 | 0    | 6.9     | 36.0   | 29406   | 402       |
| uring_multi | 4      | 4000 | 79995 | 79995 | 0    | 5.3     | 34.9   | 36565   | 227       |

### ~90% CPU

| backend     | ifaces | rate | sent  | recv  | lost | user_ms | sys_ms | vol_csw | invol_csw |
| ----------- | ------ | ---- | ----- | ----- | ---- | ------- | ------ | ------- | --------- |
| dedicated   | 4      | 4000 | 79998 | 79998 | 0    | 8.1     | 27.5   | 56204   | 94        |
| epoll       | 4      | 4000 | 79995 | 79995 | 0    | 7.7     | 35.3   | 38999   | 145       |
| recvmmsg    | 4      | 4000 | 80000 | 80000 | 0    | 3.3     | 37.8   | 38645   | 138       |
| uring       | 4      | 4000 | 79978 | 79978 | 0    | 6.4     | 31.9   | 35142   | 150       |
| uring_multi | 4      | 4000 | 80000 | 80000 | 0    | 3.4     | 27.9   | 35751   | 85        |

**NOTE:** Fewer involuntary context switches under higher CPU utilization is counter intuitive, but
correct. It means the receiver is being starved rather than interrupted. Compare the sys_ms kernel
CPU time between 75% and 90% results.

## Takeaways

* The pure CPU cost of the receivers doesn't matter nearly as much as the number of syscalls and
  context switches.
* The multipex methods (epoll, recvmmsg, and uring) are all pretty similar to each other. The
  uring_multi approach is significantly better than the rest due to batching receives. It's
  equivalent in cost if we set the batch size to 1.
* It appears all backends degrade nicely when the system is under high CPU load.
  * This is with a very cheap frame handler that exerts no backpressure
* It doesn't look like it's possible to guarantee no dropped frames
