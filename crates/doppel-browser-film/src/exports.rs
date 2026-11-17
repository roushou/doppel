//! The film's WebAssembly face, for `web/src/lib/film.ts`. Plain exports,
//! numbers in and out, so the page needs no glue but its own:
//!
//! - bytes go in through [`film_input`]: the page asks for room for `len`
//!   bytes, writes them there, then makes the call that reads them;
//! - bytes come out in one buffer, [`film_output`]; a call answers how
//!   many it put there;
//! - a film is a handle, from 1; 0 is none.
//!
//! A call that fails answers a negative number: minus the length of the
//! words saying why, which are in the output buffer.
//!
//! The module is single-threaded, as a page's WebAssembly is, so what it
//! keeps between calls is in thread-locals. Dropping a handle frees its
//! film; a page that forgets to leaves it until the page goes.
#![expect(
    unsafe_code,
    reason = "exports must keep their names (`#[unsafe(no_mangle)]`); nothing else is unsafe"
)]

use std::cell::RefCell;

use prost::Message as _;

use crate::{Film, FilmError};

thread_local! {
    /// The films, by handle less one; `None` once dropped.
    static FILMS: RefCell<Vec<Option<Film>>> = const { RefCell::new(Vec::new()) };
    /// What the page wrote for the next call.
    static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// What the last call answered.
    static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Room for `len` bytes of input, zeroed: where the page writes them.
#[unsafe(no_mangle)]
pub extern "C" fn film_input(len: u32) -> *mut u8 {
    INPUT.with_borrow_mut(|input| {
        input.clear();
        input.resize(len as usize, 0);
        input.as_mut_ptr()
    })
}

/// Where the last call's answer is.
#[unsafe(no_mangle)]
pub extern "C" fn film_output() -> *const u8 {
    OUTPUT.with_borrow(Vec::as_ptr)
}

/// Puts `bytes` in the output buffer and answers how many, as an `i32`.
fn answer(bytes: Vec<u8>) -> i32 {
    let len = i32::try_from(bytes.len()).unwrap_or(i32::MAX);
    OUTPUT.set(bytes);
    len
}

/// Puts why `error` happened in the output buffer and answers minus its length.
fn refuse(error: &FilmError) -> i32 {
    -answer(error.to_string().into_bytes())
}

/// A new film, its handle.
fn keep(film: Film) -> u32 {
    FILMS.with_borrow_mut(|films| {
        films.push(Some(film));
        u32::try_from(films.len()).unwrap_or(0)
    })
}

/// Runs `call` on film `handle` with the input; 0 or less for none.
fn with_film(handle: u32, call: impl FnOnce(&mut Film, &[u8]) -> Result<i32, FilmError>) -> i32 {
    FILMS.with_borrow_mut(|films| {
        let Some(film) = (handle as usize)
            .checked_sub(1)
            .and_then(|index| films.get_mut(index))
            .and_then(Option::as_mut)
        else {
            return -answer(format!("no film {handle}").into_bytes());
        };
        INPUT.with_borrow(|input| call(film, input).unwrap_or_else(|error| refuse(&error)))
    })
}

/// A live machine's film.
#[unsafe(no_mangle)]
pub extern "C" fn film_live() -> u32 {
    keep(Film::live())
}

/// A replay's film, of the recording in the input, whose first
/// `machine_len` bytes are the machine's name and the rest the
/// recording: its handle.
#[unsafe(no_mangle)]
pub extern "C" fn film_replay(machine_len: u32) -> i32 {
    let made = INPUT.with_borrow(|input| {
        let (machine, recording) = input.split_at(input.len().min(machine_len as usize));
        Film::replay(recording, &String::from_utf8_lossy(machine))
    });
    match made {
        Ok(film) => i32::try_from(keep(film)).unwrap_or(0),
        Err(error) => refuse(&error),
    }
}

/// Takes in the activity event in the input, at `now`.
#[unsafe(no_mangle)]
pub extern "C" fn film_told(handle: u32, now: f64) -> i32 {
    with_film(handle, |film, input| film.told(input, now).map(|()| 0))
}

/// Puts the snapshot in the input in place, for trace line `line`.
#[unsafe(no_mangle)]
pub extern "C" fn film_seen(handle: u32, line: u32) -> i32 {
    with_film(handle, |film, input| film.seen(line, input).map(|()| 0))
}

/// What goes over a `width` × `height` display at `now`, in the output.
#[unsafe(no_mangle)]
pub extern "C" fn film_frame(handle: u32, width: f64, height: f64, now: f64) -> i32 {
    with_film(handle, |film, _| {
        film.frame(width, height, now)
            .map(|frame| answer(frame.encode_to_vec()))
    })
}

/// Lets film `handle` go.
#[unsafe(no_mangle)]
pub extern "C" fn film_drop(handle: u32) {
    FILMS.with_borrow_mut(|films| {
        if let Some(film) = (handle as usize)
            .checked_sub(1)
            .and_then(|index| films.get_mut(index))
        {
            *film = None;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(len: i32) -> String {
        OUTPUT.with_borrow(|output| {
            String::from_utf8_lossy(&output[..len.unsigned_abs() as usize]).into_owned()
        })
    }

    #[test]
    fn a_failure_says_why_in_the_output() {
        let film = film_live();
        film_input(2);
        INPUT.with_borrow_mut(|input| input.copy_from_slice(&[0xff, 0xff]));
        let refused = film_told(film, 0.0);
        assert!(refused < 0);
        assert!(output(refused).starts_with("unreadable protobuf"));
        film_drop(film);
        let gone = film_told(film, 0.0);
        assert_eq!(output(gone), format!("no film {film}"));
    }

    #[test]
    fn a_frame_answers_its_length() {
        let film = film_live();
        film_input(0);
        let len = film_frame(film, 1280.0, 800.0, 0.0);
        assert!(len >= 0);
        let none = film_frame(film, 0.0, 0.0, 0.0);
        assert!(output(none).starts_with("a display without area"));
    }
}
