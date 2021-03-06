use std::io;
use std::fmt;
use std::error;
use std::str::{ FromStr, from_utf8 };
use std::num;

use {
    Value,
    ScopeType,
    Scope,
    Var,
    ScopeItem,
    SimulationCommand,
    Header,
    Command
};

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Parse(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::Io(ref err) => write!(f, "{}", err),
            Error::Parse(ref msg) => write!(f, "{}", msg),
        }
    }
}

impl error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::Io(..) => "VCD IO error",
            Error::Parse(..) => "VCD parse error",
        }
    }

    fn cause(&self) -> Option<&error::Error> {
        match *self {
            Error::Io(ref err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Error { Error::Io(err) }
}

impl From<num::ParseIntError> for Error {
    fn from(_: num::ParseIntError) -> Error { Error::Parse("Invalid number") }
}

impl From<num::ParseFloatError> for Error {
    fn from(_: num::ParseFloatError) -> Error { Error::Parse("Invalid number") }
}

impl From<::std::str::Utf8Error> for Error {
    fn from(_: ::std::str::Utf8Error) -> Error { Error::Parse("Invalid UTF8") }
}

impl From<::std::string::FromUtf8Error> for Error {
    fn from(_: ::std::string::FromUtf8Error) -> Error { Error::Parse("Invalid UTF8") }
}

fn whitespace_byte(b: u8) -> bool {
    match b {
        b' ' | b'\n' | b'\r' | b'\t' => true,
        _ => false,
    }
}

/// VCD parser. Wraps an `io::Read` and acts as an iterator of `Command`s.
pub struct Parser<R: io::Read> {
    bytes_iter: io::Bytes<R>,
    simulation_command: Option<SimulationCommand>,
}

impl<R: io::Read> Parser<R> {
    /// Create a parser wrapping an `io::Read`
    ///
    /// ```
    /// let buf = b"...";
    /// let mut vcd = vcd::Parser::new(&buf[..]);
    /// ```
    pub fn new(r: R) -> Parser<R> {
        Parser {
            bytes_iter: r.bytes(),
            simulation_command: None,
        }
    }

    fn read_byte(&mut self) -> Result<u8, Error> {
        match self.bytes_iter.next() {
            Some(Ok(b)) => Ok(b),
            Some(Err(e)) => return Err(Error::from(e)),
            None => return Err(Error::Parse("Unexpected EOF")),
        }
    }

    fn read_token<'a>(&mut self, buf: &'a mut [u8]) -> Result<&'a [u8], Error> {
        let mut len = 0;
        loop {
            let b = try!(self.read_byte());
            if whitespace_byte(b) {
                if len > 0 { break; } else { continue; }
            }

            if let Some(p) = buf.get_mut(len) {
                *p = b;
            } else {
                return Err(Error::Parse("Token too long"));
            }

            len += 1;
        }
        Ok(&buf[..len])
    }

    fn read_token_string(&mut self) -> Result<String, Error> {
        let mut r = Vec::new();
        loop {
            let b = try!(self.read_byte());
            if whitespace_byte(b) {
                if r.len() > 0 { break; } else { continue; }
            }
            r.push(b);
        }
        Ok(try!(String::from_utf8(r)))
    }

    fn read_token_parse<E, T>(&mut self) -> Result<T, Error> where Error: From<E>, T: FromStr<Err=E> {
        let mut buf = [0; 32];
        let tok = try!(self.read_token(&mut buf));

        if tok == b"$end" {
            return Err(Error::Parse("Unexpected $end"));
        }

        let s = try!(from_utf8(tok));
        Ok(try!(s.parse()))
    }

    fn read_command_end(&mut self) -> Result<(), Error> {
        let mut buf = [0; 8];
        let tok = try!(self.read_token(&mut buf));
        if tok == b"$end" { Ok(()) } else { Err(Error::Parse("Expected $end")) }
    }

    fn read_string_command(&mut self) -> Result<String, Error> {
        let mut r = Vec::new();
        loop {
            r.push(try!(self.read_byte()));
            if r.ends_with(b"$end") { break; }
        }
        let len = r.len() - 4;
        r.truncate(len);
        Ok(try!(String::from_utf8(r)).trim().to_string()) // TODO: don't reallocate
    }

    fn parse_command(&mut self) -> Result<Command, Error> {
        use super::Command::*;
        use super::SimulationCommand::*;

        let mut cmdbuf = [0; 16];
        let cmd = try!(self.read_token(&mut cmdbuf));

        match cmd {
            b"comment" => Ok(Comment(try!(self.read_string_command()))),
            b"date"    => Ok(Date(try!(self.read_string_command()))),
            b"version" => Ok(Version(try!(self.read_string_command()))),
            b"timescale" => {
                let (mut buf, mut buf2) = ([0; 8], [0; 8]);
                let tok = try!(from_utf8(try!(self.read_token(&mut buf))));
                // Support both "1ps" and "1 ps"
                let (num_str, unit_str) = match tok.find(|c: char| !c.is_numeric()) {
                    Some(idx) => (&tok[0..idx], &tok[idx..]),
                    None => (tok, try!(from_utf8(try!(self.read_token(&mut buf2)))))
                };
                try!(self.read_command_end());
                Ok(Timescale(try!(num_str.parse()), try!(unit_str.parse())))
            }
            b"scope" => {
                let scope_type = try!(self.read_token_parse());
                let identifier = try!(self.read_token_string());
                try!(self.read_command_end());
                Ok(ScopeDef(scope_type, identifier))
            }
            b"upscope" => {
                try!(self.read_command_end());
                Ok(Upscope)
            }
            b"var" => {
                let var_type = try!(self.read_token_parse());
                let size = try!(self.read_token_parse());
                let code = try!(self.read_token_parse());
                let reference = try!(self.read_token_string());
                try!(self.read_command_end());
                Ok(VarDef(var_type, size, code, reference))
            }
            b"enddefinitions" => {
                try!(self.read_command_end());
                Ok(Enddefinitions)
            }

            // Simulation commands
            b"dumpall" => self.begin_simulation_command(Dumpall),
            b"dumpoff" => self.begin_simulation_command(Dumpoff),
            b"dumpon" => self.begin_simulation_command(Dumpon),
            b"dumpvars" => self.begin_simulation_command(Dumpvars),

            b"end" => {
                if let Some(c) = self.simulation_command.take() {
                    Ok(End(c))
                } else {
                    Err(Error::Parse("Unmatched $end"))
                }
            }

            _ => Err(Error::Parse("Invalid keyword"))
        }
    }

    fn begin_simulation_command(&mut self, c: SimulationCommand) -> Result<Command, Error> {
        self.simulation_command = Some(c);
        Ok(Command::Begin(c))
    }

    fn parse_timestamp(&mut self) -> Result<Command, Error> {
        Ok(Command::Timestamp(try!(self.read_token_parse())))
    }

    fn parse_scalar(&mut self, initial: u8) ->Result<Command, Error> {
        let id = try!(self.read_token_parse());
        let val = try!(Value::parse(initial));
        Ok(Command::ChangeScalar(id, val))
    }

    fn parse_vector(&mut self) -> Result<Command, Error> {
        let mut buf = [0; 32];
        let val = try!(try!(self.read_token(&mut buf)).iter().cloned().map(Value::parse).collect());
        let id = try!(self.read_token_parse());
        Ok(Command::ChangeVector(id, val))
    }

    fn parse_real(&mut self) -> Result<Command, Error> {
        let val = try!(self.read_token_parse());
        let id = try!(self.read_token_parse());
        Ok(Command::ChangeReal(id, val))
    }

    fn parse_string(&mut self) -> Result<Command, Error> {
        let val = try!(self.read_token_string());
        let id = try!(self.read_token_parse());
        Ok(Command::ChangeString(id, val))
    }

    fn parse_scope(&mut self, scope_type: ScopeType, reference: String) -> Result<Scope, Error> {
        use super::Command::*;
        let mut children = Vec::new();

        loop {
            match self.next() {
                Some(Ok(Upscope)) => break,
                Some(Ok(ScopeDef(tp, id))) => {
                    children.push(ScopeItem::Scope(try!(self.parse_scope(tp, id))));
                }
                Some(Ok(VarDef(tp, size, id, r))) => {
                    children.push(ScopeItem::Var(
                        Var { var_type: tp, size: size, code: id, reference: r }
                    ));
                }
                Some(Ok(_)) => return Err(Error::Parse("Unexpected command in $scope")),
                Some(Err(e)) => return Err(Error::from(e)),
                None => return Err(Error::Parse("Unexpected EOF in $scope"))
            }
        }

        Ok(Scope { scope_type: scope_type, identifier: reference, children: children })
    }

    /// Parse the header of a VCD file into a `Header` struct. After returning, the stream has been
    /// read just past the `$enddefinitions` command and can be iterated to obtain the data.
    pub fn parse_header(&mut self) -> Result<Header, Error> {
        use super::Command::*;
        let mut header: Header = Default::default();
        loop {
            match self.next() {
                Some(Ok(Enddefinitions)) => break,
                Some(Ok(Comment(s))) => { header.comment = Some(s); }
                Some(Ok(Date(s)))    => { header.date    = Some(s); }
                Some(Ok(Version(s))) => { header.version = Some(s); }
                Some(Ok(Timescale(val, unit))) => { header.timescale = Some((val, unit)); }
                Some(Ok(ScopeDef(tp, id))) => {
                    header.scope = try!(self.parse_scope(tp, id));
                }
                Some(Ok(_)) => {
                    return Err(Error::Parse("Unexpected command in header"))
                }
                Some(Err(e)) => return Err(Error::from(e)),
                None => return Err(Error::Parse("Unexpected EOF in header"))
            }
        }
        Ok(header)
    }
}

impl<P: io::Read> Iterator for Parser<P> {
    type Item = Result<Command, Error>;
    fn next(&mut self) -> Option<Result<Command, Error>> {
        while let Some(b) = self.bytes_iter.next() {
            let b = match b {
                Ok(b) => b,
                Err(e) => return Some(Err(Error::from(e)))
            };
            match b {
                b' ' | b'\n' | b'\r' | b'\t' => (),
                b'$' => return Some(self.parse_command()),
                b'#' => return Some(self.parse_timestamp()),
                b'0' | b'1' | b'z' | b'Z' | b'x' | b'X' => return Some(self.parse_scalar(b)),
                b'b' | b'B' => return Some(self.parse_vector()),
                b'r' | b'R' => return Some(self.parse_real()),
                b's' | b'S' => return Some(self.parse_string()),
                _ => panic!("Unexpected character {}", b)
            }
        }
        None
    }
}

#[test]
fn wikipedia_sample() {
    use super::Command::*;
    use super::SimulationCommand::*;
    use super::Value::*;
    use super::{ TimescaleUnit, IdCode, VarType, ScopeType };

    let sample = b"
    $date
       Date text.
    $end
    $version
       VCD generator text.
    $end
    $comment
       Any comment text.
    $end
    $timescale 100 ns $end
    $scope module logic $end
    $var wire 8 # data $end
    $var wire 1 $ data_valid $end
    $var wire 1 % en $end
    $var wire 1 & rx_en $end
    $var wire 1 ' tx_en $end
    $var wire 1 ( empty $end
    $var wire 1 ) underrun $end
    $upscope $end
    $enddefinitions $end
    $dumpvars
    bxxxxxxxx #
    x$
    0%
    x&
    x'
    1(
    0)
    $end
    #0
    b10000001 #
    0$
    1%
    #2211
    0'
    #2296
    b0 #
    1$
    #2302
    0$
    #2303
        ";

    let mut b = Parser::new(&sample[..]);

    let header = b.parse_header().unwrap();
    assert_eq!(header.comment, Some("Any comment text.".to_string()));
    assert_eq!(header.date, Some("Date text.".to_string()));
    assert_eq!(header.version, Some("VCD generator text.".to_string()));
    assert_eq!(header.timescale, Some((100, TimescaleUnit::NS)));

    assert_eq!(&header.scope.identifier[..], "logic");
    assert_eq!(header.scope.scope_type, ScopeType::Module);

    if let ScopeItem::Var(ref v) = header.scope.children[0] {
        assert_eq!(v.var_type, VarType::Wire);
        assert_eq!(&v.reference[..], "data");
        assert_eq!(v.size, 8);
    } else {
        panic!("Expected Var, found {:?}", header.scope.children[0]);
    }

    let expected = &[
        Begin(Dumpvars),
        ChangeVector(IdCode(2), vec![X, X, X, X, X, X, X, X]),
        ChangeScalar(IdCode(3), X),
        ChangeScalar(IdCode(4), V0),
        ChangeScalar(IdCode(5), X),
        ChangeScalar(IdCode(6), X),
        ChangeScalar(IdCode(7), V1),
        ChangeScalar(IdCode(8), V0),
        End(Dumpvars),
        Timestamp(0),
        ChangeVector(IdCode(2), vec![V1, V0, V0, V0, V0, V0, V0, V1]),
        ChangeScalar(IdCode(3), V0),
        ChangeScalar(IdCode(4), V1),
        Timestamp(2211),
        ChangeScalar(IdCode(6), V0),
        Timestamp(2296),
        ChangeVector(IdCode(2), vec![V0]),
        ChangeScalar(IdCode(3), V1),
        Timestamp(2302),
        ChangeScalar(IdCode(3), V0),
        Timestamp(2303),
    ];

    for (i, e) in b.zip(expected.iter()) {
        assert_eq!(&i.unwrap(), e);
    }
}
