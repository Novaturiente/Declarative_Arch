[[src/main.rs:1]]
This is a program to mange archlinux packages declaratively

[[src/main.rs:174]]
* This is an attribute applied to the Config struct, asking Rust to automatically implement three traits for you:
    - Debug: Allows you to format and print the struct for debugging purposes with println!("{:?}", my_struct).
    - Deserialize: Enables your struct to be created from formats like JSON, TOML, YAML, etc., using Serde (a popular serialization library).
    - Serialize: Allows your struct to be converted into those formats (e.g., storing as JSON).

[[src/main.rs:180]]
* Explanation of Result<(), Box<dyn std::error::Error>>
    - Result<T, E> is an enum used for error handling in Rust, representing either success (Ok(T)) or failure (Err(E)).
    - Here, T is (), meaning no meaningful value is returned on success.
    - E is Box<dyn std::error::Error>, which is a boxed dynamic trait object representing any error type that implements the standard Error trait. This allows the function to return different kinds of errors through a single error type, achieved by boxing the error to handle the unknown size at compile time and allow dynamic dispatch.

* Why use Box<dyn std::error::Error>?
    - It abstracts over different error types that your function may need to return.
    - It uses dynamic dispatch so the caller can handle any error implementing the Error trait.
    - Boxing is needed because trait objects don’t have a known size at compile time, so they must be heap-allocated.

