#![deny(clippy::all, clippy::pedantic, rust_2018_idioms)]

use chrono::prelude::*;
use clap::Parser;
use std::{
    io::{self, Error, ErrorKind},
    time::Duration,
};
use tokio::time::MissedTickBehavior;
use yahoo_finance_api as yahoo;

#[derive(Parser)]
#[clap(
    version = "1.0",
    author = "Claus Matzinger",
    about = "A Manning LiveProject: async Rust"
)]
struct Opts {
    #[clap(short, long, default_value = "AAPL,MSFT,UBER,GOOG")]
    symbols: String,
    #[clap(short, long)]
    from: String,
}

///
/// A trait to provide a common interface for all signal calculations.
///
trait AsyncStockSignal {
    ///
    /// The signal's data type.
    ///
    type SignalType;

    ///
    /// Calculate the signal on the provided series.
    ///
    /// # Returns
    ///
    /// The signal (using the provided type) or `None` on error/invalid data.
    ///
    fn calculate(&self, series: &[f64]) -> Option<Self::SignalType>;
}

struct PriceDifference;
struct MinPrice;
struct MaxPrice;
struct WindowedSMA {
    window_size: usize,
}

impl AsyncStockSignal for PriceDifference {
    type SignalType = (f64, f64);

    ///
    /// Calculates the absolute and relative difference between the beginning
    /// and ending of an f64 series. The relative difference is relative to the
    /// beginning.
    ///
    /// # Returns
    ///
    /// A tuple `(absolute, relative)` difference.
    ///
    fn calculate(&self, series: &[f64]) -> Option<Self::SignalType> {
        if series.is_empty() {
            return None;
        }
        // unwrap is safe here even if first == last
        let (first, last) = (series.first().unwrap(), series.last().unwrap());
        let abs_diff = last - first;
        let first = if *first == 0.0 { 1.0 } else { *first };
        let rel_diff = abs_diff / first;
        Some((abs_diff, rel_diff))
    }
}

impl AsyncStockSignal for MinPrice {
    type SignalType = f64;

    ///
    /// Find the minimum in a series of f64
    ///
    fn calculate(&self, series: &[f64]) -> Option<Self::SignalType> {
        if series.is_empty() {
            None
        } else {
            Some(series.iter().fold(f64::MAX, |acc, q| acc.min(*q)))
        }
    }
}

impl AsyncStockSignal for MaxPrice {
    type SignalType = f64;

    ///
    /// Find the maximum in a series of f64
    ///
    fn calculate(&self, series: &[f64]) -> Option<Self::SignalType> {
        if series.is_empty() {
            None
        } else {
            Some(series.iter().fold(f64::MIN, |acc, q| acc.max(*q)))
        }
    }
}

impl AsyncStockSignal for WindowedSMA {
    type SignalType = Vec<f64>;

    ///
    /// Window function to create a simple moving average
    ///
    fn calculate(&self, series: &[f64]) -> Option<Self::SignalType> {
        if !series.is_empty() && self.window_size > 1 {
            #[allow(clippy::cast_precision_loss)]
            Some(
                series
                    .windows(self.window_size)
                    .map(|w| w.iter().sum::<f64>() / w.len() as f64)
                    .collect(),
            )
        } else {
            None
        }
    }
}

///
/// Retrieve data from a data source and extract the closing prices. Errors
/// during download are mapped onto `io::Errors` as `InvalidData`.
///
async fn fetch_closing_data(
    symbol: &str,
    beginning: &DateTime<Utc>,
    end: &DateTime<Utc>,
) -> std::io::Result<Vec<f64>> {
    let provider = yahoo::YahooConnector::new();

    let response = provider
        .get_quote_history(symbol, *beginning, *end)
        .await
        .map_err(|_| Error::from(ErrorKind::InvalidData))?;
    let mut quotes = response
        .quotes()
        .map_err(|_| Error::from(ErrorKind::InvalidData))?;
    if quotes.is_empty() {
        Ok(vec![])
    } else {
        quotes.sort_by_cached_key(|k| k.timestamp);
        Ok(quotes.iter().map(|q| q.adjclose as f64).collect())
    }
}

async fn run_symbols_report(
    symbols: Vec<String>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> io::Result<()> {
    let tasks = symbols.into_iter().map(|symbol| {
        tokio::spawn(async move {
            let closes = fetch_closing_data(&symbol, &from, &to).await?;
            process_closing_data(&symbol, &closes, &from);
            Ok(()) as io::Result<()>
        })
    });
    for result in futures_util::future::join_all(tasks).await {
        match result {
            Ok(report) => {
                if let Err(err) = report {
                    return Err(err);
                }
            }
            Err(err) => eprintln!("{:?}", err),
        }
    }
    Ok(())
}

fn process_closing_data(symbol: &str, closes: &[f64], from: &DateTime<Utc>) {
    if !closes.is_empty() {
        // min/max of the period. unwrap() because those are Option types
        let period_max: f64 = MaxPrice.calculate(closes).unwrap();
        let period_min: f64 = MinPrice.calculate(closes).unwrap();
        let last_price = *closes.last().unwrap_or(&0.0);
        let (_, pct_change) = PriceDifference.calculate(closes).unwrap_or((0.0, 0.0));
        let sma = WindowedSMA { window_size: 30 }
            .calculate(closes)
            .unwrap_or_default();

        // a simple way to output CSV data
        println!(
            "{},{},${:.2},{:.2}%,${:.2},${:.2},${:.2}",
            from.to_rfc3339(),
            symbol,
            last_price,
            pct_change * 100.0,
            period_min,
            period_max,
            sma.last().unwrap_or(&0.0)
        );
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let opts = Opts::parse();
    let from: DateTime<Utc> = opts.from.parse().expect("Couldn't parse 'from' date");
    let to = Utc::now();

    // a simple way to output a CSV header
    println!("period start,symbol,price,change %,min,max,30d avg");
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let symbols: Vec<_> = opts.symbols.split(',').map(ToString::to_string).collect();
    loop {
        interval.tick().await;
        run_symbols_report(symbols.clone(), from, to).await?;
    }
    // Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;

    #[test]
    fn test_PriceDifference_calculate() {
        let signal = PriceDifference {};
        assert_eq!(signal.calculate(&[]), None);
        assert_eq!(signal.calculate(&[1.0]), Some((0.0, 0.0)));
        assert_eq!(signal.calculate(&[1.0, 0.0]), Some((-1.0, -1.0)));
        assert_eq!(
            signal.calculate(&[2.0, 3.0, 5.0, 6.0, 1.0, 2.0, 10.0]),
            Some((8.0, 4.0))
        );
        assert_eq!(
            signal.calculate(&[0.0, 3.0, 5.0, 6.0, 1.0, 2.0, 1.0]),
            Some((1.0, 1.0))
        );
    }

    #[test]
    fn test_MinPrice_calculate() {
        let signal = MinPrice {};
        assert_eq!(signal.calculate(&[]), None);
        assert_eq!(signal.calculate(&[1.0]), Some(1.0));
        assert_eq!(signal.calculate(&[1.0, 0.0]), Some(0.0));
        assert_eq!(
            signal.calculate(&[2.0, 3.0, 5.0, 6.0, 1.0, 2.0, 10.0]),
            Some(1.0)
        );
        assert_eq!(
            signal.calculate(&[0.0, 3.0, 5.0, 6.0, 1.0, 2.0, 1.0]),
            Some(0.0)
        );
    }

    #[test]
    fn test_MaxPrice_calculate() {
        let signal = MaxPrice {};
        assert_eq!(signal.calculate(&[]), None);
        assert_eq!(signal.calculate(&[1.0]), Some(1.0));
        assert_eq!(signal.calculate(&[1.0, 0.0]), Some(1.0));
        assert_eq!(
            signal.calculate(&[2.0, 3.0, 5.0, 6.0, 1.0, 2.0, 10.0]),
            Some(10.0)
        );
        assert_eq!(
            signal.calculate(&[0.0, 3.0, 5.0, 6.0, 1.0, 2.0, 1.0]),
            Some(6.0)
        );
    }

    #[test]
    fn test_WindowedSMA_calculate() {
        let series = vec![2.0, 4.5, 5.3, 6.5, 4.7];

        let signal = WindowedSMA { window_size: 3 };
        assert_eq!(
            signal.calculate(&series),
            Some(vec![3.933_333_333_333_333_6, 5.433_333_333_333_334, 5.5])
        );

        let signal = WindowedSMA { window_size: 5 };
        assert_eq!(signal.calculate(&series), Some(vec![4.6]));

        let signal = WindowedSMA { window_size: 10 };
        assert_eq!(signal.calculate(&series), Some(vec![]));
    }
}
