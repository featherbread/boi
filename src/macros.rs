//! Internal macros for use throughout this tool.

/// Prints to standard error with basic log formatting and a newline.
macro_rules! speak {
    ($fmt:literal $(, $($args:tt)* )?) => {{
        use ::std::io::Write;
        let _ = writeln!(
            ::std::io::stderr().lock(),
            "[{}] {}",
            env!("CARGO_PKG_NAME"),
            format_args!($fmt $(, $($args)* )?),
        );
    }};
}

/// [`speak`]s to standard error, then terminates the current process with an exit code.
macro_rules! die {
    (code = $code:expr, $fmt:literal $(, $($args:tt)* )?) => {{
        speak!("{}", format_args!($fmt $(, $($args)* )?));
        ::std::process::exit($code);
    }};
    ($fmt:literal $(, $($args:tt)* )?) => {{
        die!(code = 1, $fmt $(, $($args)* )?);
    }};
}
