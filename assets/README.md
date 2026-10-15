# Example

## Bash

```bash
echo "Hello, world!"
```

## Python

```python
print("Hello, world!")
```

## Ruby

This code block runs with Ruby.

```ruby
puts "Hello, world!"
```

## JavaScript

This code block runs with Node.js.

```javascript
console.log("Hello, world!");
```

## Go

This code block runs with Go.

```go
package main
import "fmt"
func main() {
    fmt.Println("Hello, world!")
}
```

## Rust

```rust
fn main() {
    println!("Hello, world!");
}
```

## Perl

```perl
print "Hello, world!\n";
```

## mq

```mq
print("Hello, world!")
```

## build

```bash
echo "Building the project..."
```

## test

```meta
depends = ["build"]
```

```bash
echo "Running tests..."
```

## deploy

```meta
depends = ["test"]
params = ["env=staging"]
```

```bash
echo "Deploying to $MX_PARAM_ENV..."
```

## lint

```bash
echo "Linting..."
sleep 1.5
```

## compile

```bash
echo "Compiling..."
sleep 1
```

## unit

```meta
depends = ["compile"]
```

```bash
echo "Running unit tests..."
sleep 2
```

## e2e

```meta
depends = ["compile"]
```

```bash
echo "Running e2e tests..."
sleep 2.5
```

## ci

```meta
depends = ["lint", "unit", "e2e"]
parallel = true
```

```bash
echo "All checks passed"
```
