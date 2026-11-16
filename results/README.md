# Measured results

Produced by the scripts in `eval/`; see the main README for how to run them.

## Rebuilding the real order books (NASDAQ TotalView-ITCH 5.0, 2019-01-30)

| | C++ (engine's book) | Go (market-data books) |
|---|---:|---:|
| messages | 368,366,634 | 368,366,634 |
| orders added | 164,696,353 | 164,696,353 |
| largest number of live orders | 1,739,902 | – |
| live orders at the end of the day | 0 | 0 |
| messages naming an unknown order | 0 | 0 |
| executions or cancels over the open quantity | 0 | 0 |
| book checks in regular hours | 186,676,286 | 186,676,286 |
| crossed books | 0 | 0 |
| locked books | 0 | 0 |
| shares executed against displayed orders | 1,119,323,998 | 1,119,323,998 |
| shares executed against hidden orders | 173,244,448 | 173,244,448 |
| seconds | 72 | 166 |
| messages per second | 5,135,099 | 2,215,119 |

Matching priority: consecutive executions with the same timestamp against one side of one stock come from one incoming order. For each such group in regular trading, the engine was asked which resting orders an order of that size and limit would hit.

| priority within a price level | groups checked | same orders and quantities as NASDAQ | different |
|---|---:|---:|---:|
| by order reference number (entry order) | 5,195,904 | 5,195,904 | 0 |
| by arrival of the add message | 5,195,904 | 5,193,912 | 1,992 |

## Latency and throughput: Windows-11-10.0.26300-SP0

### Matching engine

| | commands/s | p50 | p90 | p99 | p99.9 | p99.99 | max |
|---|---:|---:|---:|---:|---:|---:|---:|
| in process | 12.05 M | 0.09 µs | 0.13 µs | 0.36 µs | 0.50 µs | 1.28 µs | 95.00 µs |
| bus, 0.1 M/s offered | 0.10 M | 0.44 µs | 0.63 µs | 19.16 µs | 640.90 µs | 6,520.40 µs | 8,766.33 µs |
| bus, 1.0 M/s offered | 1.00 M | 0.37 µs | 0.51 µs | 18.33 µs | 76.92 µs | 2,171.61 µs | 2,487.45 µs |
| bus, 2.0 M/s offered | 2.00 M | 0.36 µs | 0.52 µs | 22.23 µs | 1,380.85 µs | 2,653.64 µs | 2,771.06 µs |
| bus, 4.0 M/s offered | 4.00 M | 0.38 µs | 15.47 µs | 235.26 µs | 2,017.88 µs | 2,328.18 µs | 2,346.01 µs |
| bus, as fast as possible | 6.54 M | 5,314.44 µs | 9,019.66 µs | 10,574.00 µs | 11,614.32 µs | 12,140.83 µs | 12,199.70 µs |

### Order entry over TCP

Plain TCP round trip on loopback (49 bytes, echo): p50 45.9 µs, p99 114.7 µs, p99.9 170.9 µs.

| protocol | orders/s | round trip p50 | p99 | p99.9 | inside the gateway p50 | p99 | p99.9 |
|---|---:|---:|---:|---:|---:|---:|---:|
| OUCH | 1,000 | 108.8 | 237.5 | 304.0 | 88.8 | 184.3 | 247.1 |
| OUCH | 10,000 | 91.0 | 202.7 | 269.3 | 80.3 | 163.9 | 218.5 |
| OUCH | 50,000 | 157.2 | 373.2 | 51,715.4 | 114.3 | 325.4 | 51,730.2 |
| OUCH | 100,000 | 182.1 | 415.7 | 970.0 | 134.9 | 360.5 | 521.7 |
| FIX | 1,000 | 139.9 | 283.7 | 349.3 | 109.6 | 238.5 | 313.9 |
| FIX | 10,000 | 121.4 | 257.0 | 353.2 | 94.9 | 215.3 | 297.7 |
| FIX | 50,000 | 308.0 | 915.1 | 1,391.2 | 215.1 | 556.1 | 824.2 |

### Market data fan-out (MoldUDP64, 20,000 orders/s flowing)

| subscribers | dropped on purpose | messages/s delivered | latency p50 | p99 | p99.9 | gaps | recovered | missing |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 0% | 16,166 | 109.3 µs | 339.3 µs | 1,169.8 µs | 0 | 0 | 0 |
| 10 | 0% | 161,664 | 384.7 µs | 803.8 µs | 4,176.3 µs | 10 | 30 | 0 |
| 50 | 0% | 808,322 | 1,625 µs | 4,386.2 µs | 7,571.8 µs | 0 | 0 | 0 |
| 100 | 0% | 1,525,399 | 1,656,079 µs | 2,811,474.2 µs | 2,815,559 µs | 1,367 | 4,725 | 0 |
| 10 | 1% | 160,094 | 443.1 µs | 856.3 µs | 3,101.3 µs | 5,442 | 31,606 | 0 |

## Latency and throughput: Linux-6.18.58.1-microsoft-standard-WSL2-x86_64-with-glibc2.39

### Matching engine

| | commands/s | p50 | p90 | p99 | p99.9 | p99.99 | max |
|---|---:|---:|---:|---:|---:|---:|---:|
| in process | 10.32 M | 0.10 µs | 0.14 µs | 0.38 µs | 0.52 µs | 12.79 µs | 85.96 µs |
| bus, 0.1 M/s offered | 0.10 M | 0.43 µs | 0.70 µs | 20.85 µs | 55.08 µs | 825.81 µs | 4,342.88 µs |
| bus, 1.0 M/s offered | 1.00 M | 0.32 µs | 0.77 µs | 31.06 µs | 4,201.48 µs | 6,792.96 µs | 7,016.29 µs |
| bus, 2.0 M/s offered | 2.00 M | 0.31 µs | 3.12 µs | 36.31 µs | 2,153.42 µs | 3,251.56 µs | 3,337.01 µs |
| bus, 4.0 M/s offered | 4.00 M | 0.35 µs | 72.73 µs | 1,887.20 µs | 4,302.23 µs | 4,434.81 µs | 4,446.05 µs |
| bus, as fast as possible | 5.06 M | 7,529.35 µs | 12,285.04 µs | 14,197.11 µs | 14,922.61 µs | 15,443.37 µs | 15,536.53 µs |

### Order entry over TCP

Plain TCP round trip on loopback (49 bytes, echo): p50 57.0 µs, p99 140.6 µs, p99.9 200.1 µs.

| protocol | orders/s | round trip p50 | p99 | p99.9 | inside the gateway p50 | p99 | p99.9 |
|---|---:|---:|---:|---:|---:|---:|---:|
| OUCH | 1,000 | 56.4 | 136.0 | 277.9 | 25.1 | 85.3 | 184.0 |
| OUCH | 10,000 | 45.4 | 123.3 | 159.5 | 21.5 | 74.5 | 114.6 |
| OUCH | 50,000 | 39.6 | 109.7 | 150.0 | 19.4 | 58.1 | 105.0 |
| OUCH | 100,000 | 71.2 | 190.6 | 281.6 | 18.1 | 96.7 | 154.4 |
| FIX | 1,000 | 80.7 | 163.3 | 229.1 | 45.6 | 120.6 | 173.3 |
| FIX | 10,000 | 60.5 | 132.3 | 177.7 | 32.3 | 97.0 | 137.5 |
| FIX | 50,000 | 53.2 | 158.9 | 225.3 | 26.4 | 104.7 | 170.0 |

### Market data fan-out (MoldUDP64, 20,000 orders/s flowing)

| subscribers | dropped on purpose | messages/s delivered | latency p50 | p99 | p99.9 | gaps | recovered | missing |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 0% | 16,166 | 151.6 µs | 227 µs | 2,641.6 µs | 0 | 0 | 0 |
| 10 | 0% | 161,666 | 285.5 µs | 3,948.8 µs | 5,718.5 µs | 0 | 0 | 0 |
| 50 | 0% | 808,352 | 412.5 µs | 3,731 µs | 9,888.8 µs | 0 | 0 | 0 |
| 100 | 0% | 1,616,794 | 1,129.9 µs | 6,549.9 µs | 26,966.8 µs | 59 | 521 | 0 |
| 10 | 1% | 160,082 | 249.7 µs | 3,903.6 µs | 5,436.5 µs | 9,216 | 31,670 | 0 |

## The whole exchange end to end

A simulated market of 600 s (121,773 commands, 7,892 trades) replayed through OUCH to the running exchange at 9,623 commands/s (each command after the previous one's acknowledgment):

- trades identical: yes
- market data book identical: yes
- clearing positions identical: yes
- settlement balanced: yes
- market data: 0 unknown orders, 0 crossed books; clearing booked 7,892 trades

## Statistical realism of the simulated market

| fact | AAPL | MSFT | AMD | INTC | QQQ | simulated, rule-based makers | simulated, with the learned maker |
|---|---|---|---|---|---|---|---|
| heavy tails (10 s kurtosis > 1) | yes | yes | yes | yes | yes | yes | yes |
| no return autocorrelation (|acf| < 0.1) | yes | yes | yes | yes | yes | yes | yes |
| volatility clustering (|r| acf > 0.05) | yes | yes | yes | yes | yes | yes | yes |
| long memory of trade signs (exponent < 1) | yes | yes | yes | yes | yes | yes | yes |
| tight spread (mostly <= 2 ticks) | yes | yes | yes | yes | yes | yes | yes |
| positive, concave impact (exponent < 1) | yes | yes | yes | yes | yes | yes | yes |
| bursty trading (inter-arrival CV > 1) | yes | yes | yes | yes | yes | yes | yes |
| fat-tailed order placement (tail exponent < 3) | yes | yes | yes | yes | yes | yes | yes |

| statistic | AAPL | MSFT | AMD | INTC | QQQ | simulated, rule-based makers | simulated, with the learned maker |
|---|---:|---:|---:|---:|---:|---:|---:|
| trades per minute | 216 | 161 | 250 | 72 | 132 | 839 | 828 |
| volatility over 10 s (ticks) | 5.28 | 2.35 | 1.93 | 1.26 | 3.04 | 6.45 | 3.08 |
| kurtosis of 10 s returns | 4.4 | 27.1 | 5.6 | 18.5 | 69.6 | 420.0 | 255.7 |
| Hill tail exponent of 10 s returns | 3.54 | 2.97 | 4.28 | 5.13 | 3.10 | 1.40 | 1.97 |
| autocorrelation of |10 s returns|, lags 1-30 | 0.101 | 0.115 | 0.179 | 0.062 | 0.102 | 0.158 | 0.241 |
| |autocorrelation| of 10 s returns, lags 1-30 | 0.022 | 0.023 | 0.028 | 0.017 | 0.020 | 0.038 | 0.044 |
| decay exponent of the trade-sign autocorrelation | 0.53 | 0.64 | 0.84 | 0.67 | 0.65 | 0.10 | 0.09 |
| trade-sign autocorrelation at lag 1 | 0.61 | 0.63 | 0.72 | 0.71 | 0.57 | 0.73 | 0.73 |
| mean spread (ticks) | 2.25 | 1.09 | 1.03 | 1.01 | 1.15 | 1.32 | 1.17 |
| impact of a trade after 10 s (ticks) | 1.89 | 0.83 | 0.75 | 0.63 | 0.60 | 0.36 | 0.27 |
| coefficient of variation of inter-trade times | 2.90 | 3.32 | 4.50 | 3.23 | 2.89 | 2.45 | 2.41 |
| limit orders at the best price or inside | 0.35 | 0.60 | 0.63 | 0.60 | 0.70 | 0.49 | 0.49 |
| tail exponent of limit-order distances | 0.56 | 0.92 | 0.99 | 0.45 | 1.94 | 1.07 | 1.04 |

Kolmogorov-Smirnov distance between normalized 10 s returns: simulated, rule-based makers vs the five real stocks 0.424, simulated, with the learned maker vs the five real stocks 0.467; each real stock against the four others: AAPL 0.128, MSFT 0.217, AMD 0.278, INTC 0.218, QQQ 0.213.

## Market makers: learned (PPO) against rules, same seat, unseen days

| maker | mean P&L per half hour ($) | std ($) | profitable episodes | mean abs. inventory (shares) | market spread (ticks) |
|---|---:|---:|---:|---:|---:|
| learned (PPO) | -236 | 595 | 40% | 98 | 1.03 |
| Avellaneda-Stoikov | -439 | 811 | 30% | 211 | 1.22 |
| fixed, at the touch | -2,323 | 3,871 | 40% | 3,686 | 1.06 |

What the learned policy does (share of decisions): h=3.5 skew=-2: 39%, h=3.5 skew=0: 36%, h=2.5 skew=0: 21%, h=1.5 skew=-1: 4%, h=0.5 skew=-1: 0%.

## Calibration

Random search over 401 parameter sets (two half-hour simulations each): distance to the real statistics 97.70 with the hand-set defaults, 6.07 after calibration.

```
{
 "p0": 1000000,
 "limit_rate": 122.67938,
 "market_rate": 1.85493,
 "cancel_rate": 0.1223,
 "mu": 0.96065,
 "max_distance": 200,
 "meta_rate": 0.15,
 "meta_alpha": 1.5,
 "meta_child_rate": 0.6773,
 "fund_rate": 2.83437,
 "fund_vol": 6e-05,
 "vol_of_vol": 1.23626,
 "vol_tau": 258.40245,
 "fund_strength": 0.1691,
 "chart_rate": 0.99237,
 "chart_window": 30.0,
 "chart_threshold": 0.00161,
 "lot_p": 0.60001,
 "market_lot_p": 0.22691,
 "hawkes_jump": 4e-05,
 "hawkes_tau": 773.3769,
 "activity_vol": 1.5,
 "activity_tau": 1440.55186
}
```

