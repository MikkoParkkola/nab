use std::time::Instant;

use anyhow::Result;

use nab::AcceleratedClient;

pub async fn cmd_bench(urls: &str, iterations: usize) -> Result<()> {
    let client = AcceleratedClient::new()?;
    let urls: Vec<&str> = urls.split(',').map(str::trim).collect();

    println!(
        "🚀 Benchmarking {} URLs, {} iterations each\n",
        urls.len(),
        iterations
    );

    for url in urls {
        let mut times = Vec::with_capacity(iterations);

        for i in 0..iterations {
            let start = Instant::now();
            let response = client.fetch(url).await?;
            let _ = response.text().await?;
            let elapsed = start.elapsed();
            times.push(elapsed.as_secs_f64() * 1000.0);

            print!(".");
            if i == iterations - 1 {
                println!();
            }
        }

        let avg = times.iter().sum::<f64>() / times.len() as f64;
        let min = times.iter().copied().fold(f64::INFINITY, f64::min);
        let max = times.iter().copied().fold(f64::NEG_INFINITY, f64::max);

        println!("📊 {url}");
        println!("   Avg: {avg:.2}ms | Min: {min:.2}ms | Max: {max:.2}ms\n");
    }

    Ok(())
}
