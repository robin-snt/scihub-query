# `scihub-query`

This is a rudimentary tool for querying [scihub](https://scihub.copernicus.eu/dhus/search).

Currently this tool is an MVP with only the most basic features and has no optimizations and probably some bugs.

## Install

If you don't have access to a pre-compiled binary, you can install from this repo:

```sh
cargo install scihub-query
```

## Usage

```sh
scihub-query --help
```

## Configuration

`scihub-query` requires scihub credentials to work. These can be written manually to `~/.config/scihub-query/scihub-query.toml`, or entered by providing the `-s` flag.

The config file format is as follows:

```toml
username = 'john'
password = 'my-secret-scihub-password'
```

You will be prompted to enter your credentials if they are not found.

## Desired features

- [ ] Asynchronous calls for paginated responses.
- [ ] Improved parameterization of query inputs.
- [ ] Converting the entire XML response into JSON.
- [ ] POST requests for large AOI's if scihub supports it.
