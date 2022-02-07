# drift-liquidator

A liquidator for [Drift](https://www.drift.trade/).

## Usage

First, use your Solana private key to create an account on the [Drift platform](https://app.drift.trade/) by connecting your wallet and depositing some collateral. Now you're ready to run your liquidator!

You can either run the liquidator on the host with rust installed or in Docker.

To run this with rust, ensure you have [rust installed](https://www.rust-lang.org/tools/install). Then do:
```
cargo r --release -- --keypath /path/to/your/key
```

For example, on Linux/MacOS you can do:
```
cargo r --release -- --keypath $HOME/.config/solana/cli/config.yml
```

To see the full list options, do `cargo r --release -- --help`.

To run this with docker, ensure you have [Docker installed](https://docs.docker.com/get-docker/)
```
KEYPATH=/path/to/your/key ./docker-run
```

## Internals

This liquidator interacts directly with the rust functions onchain in <https://github.com/drift-labs/protocol-v1>. This allows us to avoid reimplementing the liquidation calculations, ensuring it stays in sync with the source of truth on-chain.

The logic mirrors the liquidate function in [https://github.com/drift-labs/protocol-v1/blob/master/programs/clearing_house/src/lib.rs](lib.rs), calling `setle_funding_payment` and `calculate_margin_ratio`.

If the liquidator stops working, the onchain program might have changed. In this case, try running `cargo update`.
