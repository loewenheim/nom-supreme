//! Additional parser combinators for running subparsers in a loop. Generally
//! designed to try to provide more useful errors by being aware of a terminal
//! condition.

use std::convert::Infallible;

use nom::{
    error::{append_error, ErrorKind::SeparatedNonEmptyList, FromExternalError, ParseError},
    Err::{Error, Failure, Incomplete},
    Parser,
};

fn make_infallible<A, B>(
    mut func: impl FnMut(A, B) -> A,
) -> impl FnMut(A, B) -> Result<A, Infallible> {
    move |a, b| Ok(func(a, b))
}

/// The perfected folding parser. Parses a series of 1 more more things,
/// separated by some `separator`, terminated by some `terminator`, folding
/// all of them together.
///
/// When parsing begins, an accumulator value is created with init(). Then,
/// each parsed item will be folded into the accumulator via the `fold`
/// function. After parsing each item, `parse_separated_terminated` will
/// attempt to parse a `terminator`. If it succeeds, it will return the
/// accumulator; otherwise, it will attempt to parse a separator. If it fails
/// to parse either a separator or a terminator, it will return an error;
/// otherwise, it will continue on to parse and fold the next item.
///
/// If you want 0 or more things to be parsed, wrap this in
/// [`opt`][crate::parser_ext::ParserExt::opt] or [`alt`][nom::branch::alt].
///
/// This parser will stop as soon as it finds a `terminator`. If you wish to
/// have a `terminator` parser that is the same as your `separator`, you'll
/// need to add some extra context to the terminator parser; perhaps a
/// lookahead with [`peek`][crate::parser_ext::ParserExt::peek].
///
/// This parser exists to provide meaningful parse errors. By requiring a
/// terminator, we can ensure that it doesn't suffer from the normal folding
/// parser problem of unconditionally returning success because all parse
/// failures simply end the fold without knowing if there's a larger problem.
///
/// `parse_separated_terminated` will attempt to smartly allow 0-length
/// matches. It will allow subparsers to have 0-length matches, but if a full
/// loop is made without any progress being made, it will return an error.
#[inline]
pub fn parse_separated_terminated<Input, ParseOutput, SepOutput, TermOutput, ParseErr, Accum>(
    parser: impl Parser<Input, ParseOutput, ParseErr>,
    separator: impl Parser<Input, SepOutput, ParseErr>,
    terminator: impl Parser<Input, TermOutput, ParseErr>,

    init: impl FnMut() -> Accum,
    fold: impl FnMut(Accum, ParseOutput) -> Accum,
) -> impl Parser<Input, Accum, ParseErr>
where
    Input: Clone + PartialEq,
    ParseErr: ParseError<Input>,
{
    parse_separated_terminated_impl(
        parser,
        separator,
        terminator,
        init,
        make_infallible(fold),
        |_input, _err| unreachable!(),
    )
}

/// The perfected folding parser. Parses a series of 1 more more things,
/// separated by some `separator`, terminated by some `terminator`, folding
/// all of them together with a fallible fold function.
///
/// This function is identical to [`parse_separated_terminated`], except that
/// the fold function may return an error. See its documentation for more
/// details about the precise behavior of this parser.
#[inline]
pub fn parse_separated_terminated_res<
    Input,
    ParseOutput,
    SepOutput,
    TermOutput,
    ParseErr,
    Accum,
    FoldErr,
>(
    parser: impl Parser<Input, ParseOutput, ParseErr>,
    separator: impl Parser<Input, SepOutput, ParseErr>,
    terminator: impl Parser<Input, TermOutput, ParseErr>,

    init: impl FnMut() -> Accum,
    fold: impl FnMut(Accum, ParseOutput) -> Result<Accum, FoldErr>,
) -> impl Parser<Input, Accum, ParseErr>
where
    Input: Clone + PartialEq,
    ParseErr: ParseError<Input> + FromExternalError<Input, FoldErr>,
{
    parse_separated_terminated_impl(parser, separator, terminator, init, fold, |input, err| {
        ParseErr::from_external_error(input, SeparatedNonEmptyList, err)
    })
}

/// Helper enum for tracking zero length parses. `parse_separated_terminated`
/// allows for subparsers to return zero-length matches, but if *every*
/// subparser does so in a loop, that's reported as an error.
///
/// This enum specifically tracks the least-recent zero-length parse that has
/// not been succeeded by a non-zero-length parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZeroLengthParseState {
    None,
    Item,
    Separator,
}

fn maybe_attach_error<I, E: ParseError<I>>(err1: E, err2: Option<E>) -> E {
    match err2 {
        None => err1,
        Some(err2) => err1.or(err2),
    }
}

/// Shared implementation for parse_separated_terminated_res and
/// parse_separated_terminated. This exists so that we don't have to have an
/// unnecessary bound of FromExternalError on parse_separated_terminated.
#[inline]
fn parse_separated_terminated_impl<
    Input,
    ParseOutput,
    SepOutput,
    TermOutput,
    ParseErr,
    Accum,
    FoldErr,
>(
    mut parser: impl Parser<Input, ParseOutput, ParseErr>,
    mut separator: impl Parser<Input, SepOutput, ParseErr>,
    mut terminator: impl Parser<Input, TermOutput, ParseErr>,

    mut init: impl FnMut() -> Accum,
    mut fold: impl FnMut(Accum, ParseOutput) -> Result<Accum, FoldErr>,

    mut build_error: impl FnMut(Input, FoldErr) -> ParseErr,
) -> impl Parser<Input, Accum, ParseErr>
where
    Input: Clone + PartialEq,
    ParseErr: ParseError<Input>,
{
    move |mut input: Input| {
        let mut accum = init();

        let mut zero_length_state = ZeroLengthParseState::None;
        let mut terminator_error = None;

        // TODO: various kinds of 0-length tracking:
        // - If we go a full loop without making any progress, that's an error
        // - If the separator matches a 0-length match, and the parser fails,
        //   include the most recent terminator error along with the parser
        //   parser error. It *might* be worth doing this even if separator
        //   is non-zero length? For example, suppose the separator is , and
        //   the terminator is , with a peeking lookahead. It might be worth
        //   knowing that the terminator was tried and failed there.
        loop {
            // Try to find a value. To fail to do so at this point is an
            // error, since we either just started or successfully parsed a
            // separator.
            //
            // If an error occurs here, also try to attach terminator_error.
            // terminator_error is available if the most recent separator parse
            // was zero-length, which means that both the terminator and the
            // item would be valid parses at this point.
            let (tail, value) = match parser.parse(input.clone()) {
                Ok(success) => success,
                Err(err) => {
                    break Err(err.map(move |err| {
                        append_error(
                            input,
                            SeparatedNonEmptyList,
                            maybe_attach_error(err, terminator_error),
                        )
                    }))
                }
            };

            // Check zero-length matches
            match (input == tail, zero_length_state) {
                // If both the item and the separator had a zero length match,
                // we're hanging. Bail.
                (true, ZeroLengthParseState::Separator) => {
                    break Err(Error(ParseErr::from_error_kind(
                        input,
                        SeparatedNonEmptyList,
                    )))
                }

                // If only the item had a zero-length match, update the
                // state.
                (true, _) => zero_length_state = ZeroLengthParseState::Item,

                // If the item had a non-zero length match, clear the state
                (false, _) => zero_length_state = ZeroLengthParseState::None,
            }

            // Advance the loop state
            accum = fold(accum, value).map_err(|err| Error(build_error(input, err)))?;
            input = tail;

            // Try to find a terminator; if we found it, we're done. If we
            // didn't, preserve the error, so that it can be reported as an
            // .or() branch with the subsequent separator or item error.
            let term_err = match terminator.parse(input.clone()) {
                // We found a terminator, so we're done
                Ok((tail, _)) => break Ok((tail, accum)),

                // No terminator. Keep track of the error in case we also fail
                // to find a separator or item.
                Err(Error(err)) => err,

                // Other kinds of errors should be returned immediately.
                Err(err) => {
                    break Err(
                        err.map(move |err| ParseErr::append(input, SeparatedNonEmptyList, err))
                    )
                }
            };

            // No terminator, so instead try to find a separator
            let tail = match separator.parse(input.clone()) {
                Ok((tail, _)) => tail,
                Err(Error(err)) => {
                    break Err(Error(append_error(
                        input,
                        SeparatedNonEmptyList,
                        ParseErr::or(err, term_err),
                    )))
                }
                Err(Failure(err)) => {
                    break (Err(Failure(append_error(input, SeparatedNonEmptyList, err))))
                }
                Err(Incomplete(n)) => break Err(Incomplete(n)),
            };

            // Check zero-length matches
            match (input == tail, zero_length_state) {
                // If both the separator and the item had a zero length match,
                // we're hanging. Bail.
                (true, ZeroLengthParseState::Item) => {
                    break Err(Error(ParseErr::from_error_kind(
                        input,
                        SeparatedNonEmptyList,
                    )))
                }
                // If only the separator had a zero-length match, update the
                // state. Additionally preserve the terminator error so that
                // it can be reported as an alternate if there was an item
                // error.
                (true, _) => {
                    zero_length_state = ZeroLengthParseState::Separator;
                    terminator_error = Some(term_err);
                }
                // If the separator had a non-zero length match, clear the
                // state
                (false, _) => {
                    zero_length_state = ZeroLengthParseState::None;
                    terminator_error = None;
                }
            }

            // Advance the loop state
            input = tail;
        }
    }
}

#[cfg(test)]
mod test_separated_terminated {
    use cascade::cascade;
    use cool_asserts::assert_matches;
    use nom::{
        branch::alt,
        character::complete::{alpha0, char, digit1, space0},
        error::ErrorKind,
        Err, IResult, Parser,
    };

    use crate::parser_ext::ParserExt;
    use crate::{
        error::{BaseErrorKind, ErrorTree, Expectation, StackContext},
        parse_from_str,
    };

    use super::parse_separated_terminated;

    /// Parse a series of numbers, separated by commas, terminated by a period.
    fn parse_number_list(input: &str) -> IResult<&str, Vec<i64>, ErrorTree<&str>> {
        parse_separated_terminated(
            parse_from_str(digit1),
            char(',').delimited_by(space0),
            char('.').preceded_by(space0),
            Vec::new,
            |vec, num| cascade! {vec; ..push(num);},
        )
        .parse(input)
    }

    #[test]
    fn basic() {
        assert_eq!(
            parse_number_list("1, 2, 3, 4, 5.").unwrap(),
            ("", vec![1, 2, 3, 4, 5]),
        )
    }

    #[test]
    fn trailing_input() {
        assert_eq!(
            parse_number_list("1, 2, 3, 4, 5. 4, 5, 6.").unwrap(),
            (" 4, 5, 6.", vec![1, 2, 3, 4, 5]),
        )
    }

    #[test]
    fn only_one() {
        assert_eq!(parse_number_list("10.").unwrap(), ("", vec![10]),)
    }

    #[test]
    fn at_least_one() {
        let err = parse_number_list("abc").unwrap_err();

        assert_matches!(
            err,
            Err::Error(ErrorTree::Stack{contexts, base}) => {
                assert_eq!(contexts, [("abc", StackContext::Kind(ErrorKind::SeparatedNonEmptyList))]);
                assert_matches!(
                    *base,
                    ErrorTree::Base{location: "abc", kind: BaseErrorKind::Expected(Expectation::Digit)},
                );
            }
        );
    }

    /// Test that a parse failure from both the separator and the terminator
    /// causes an error including both messages
    #[test]
    fn terminator_separator_miss() {
        let err = parse_number_list("10, 20 30.").unwrap_err();

        assert_matches!(err, Err::Error(ErrorTree::Stack{contexts, base}) => {
            assert_eq!(contexts, [(" 30.", StackContext::Kind(ErrorKind::SeparatedNonEmptyList))]);
            assert_matches!(*base, ErrorTree::Alt(choices) => assert_matches!(choices.as_slice(), [
                ErrorTree::Base{location: "30.", kind: BaseErrorKind::Expected(Expectation::Char(','))},
                ErrorTree::Base{location: "30.", kind: BaseErrorKind::Expected(Expectation::Char('.'))},
            ]));
        });
    }

    /// Test that a terminator is required, even at EoF
    #[test]
    fn required_terminator() {
        let err = parse_number_list("1, 2, 3").unwrap_err();

        assert_matches!(err, Err::Error(ErrorTree::Stack{contexts, base}) => {
            assert_eq!(contexts, [("", StackContext::Kind(ErrorKind::SeparatedNonEmptyList))]);
            assert_matches!(*base, ErrorTree::Alt(choices) => assert_matches!(
                choices.as_slice(),
                [
                    ErrorTree::Base {
                        location: "",
                        kind: BaseErrorKind::Expected(Expectation::Char(','))
                    },
                    ErrorTree::Base {
                        location: "",
                        kind: BaseErrorKind::Expected(Expectation::Char('.'))
                    },
                ]
            ));
        });
    }

    /// Test that a parse failure from the item parser includes only that error
    /// if the separator isn't zero-length
    #[test]
    fn item_error() {
        let err = parse_number_list("1, 2, abc.").unwrap_err();

        assert_matches!(err, Err::Error(ErrorTree::Stack{base, contexts}) => {
            assert_eq!(contexts, [("abc.", StackContext::Kind(ErrorKind::SeparatedNonEmptyList))]);
            assert_matches!(*base, ErrorTree::Base {
                location: "abc.",
                kind: BaseErrorKind::Expected(Expectation::Digit),
            });
        });
    }

    /// Parse a series of numbers ending in periods, separated by 0 or more
    /// whitespace, terminated by a semicolon. Used to test 0-length
    /// separator behavior.
    fn parse_number_dot_list(input: &str) -> IResult<&str, Vec<i64>, ErrorTree<&str>> {
        parse_separated_terminated(
            digit1.parse_from_str().terminated(char('.')),
            space0,
            char(';'),
            Vec::new,
            |vec, num| cascade! {vec; ..push(num);},
        )
        .parse(input)
    }

    #[test]
    fn zero_length_separator() {
        assert_eq!(
            parse_number_dot_list("1.2. 3.4.  5.; abc").unwrap(),
            (" abc", vec![1, 2, 3, 4, 5])
        );
    }

    /// Test that, when a separator matches zero length, and then the item
    /// parser fails, the returned error includes both the item error and the
    /// terminator error.
    #[test]
    fn zero_length_separator_item_term_error() {
        let err = parse_number_dot_list("1.2.3.abc.;").unwrap_err();

        assert_matches!(err, Err::Error(ErrorTree::Stack{contexts, base}) => {
            assert_eq!(contexts, [("abc.;", StackContext::Kind(ErrorKind::SeparatedNonEmptyList))]);
            assert_matches!(*base, ErrorTree::Alt(choices) => assert_matches!(
                choices.as_slice(),
                [
                    ErrorTree::Base {
                        location: "abc.;",
                        kind: BaseErrorKind::Expected(Expectation::Digit)
                    },
                    ErrorTree::Base {
                        location: "abc.;",
                        kind: BaseErrorKind::Expected(Expectation::Char(';'))
                    },
                ]
            ));
        });
    }

    /// Parse a series of runs of 1 or more digits or 0 more more letters, separated by
    /// an optional dash, terminated by a semicolon. Used to test
    /// infinite loop detection
    fn parse_letters_numbers(input: &str) -> IResult<&str, Vec<&str>, ErrorTree<&str>> {
        parse_separated_terminated(
            alt((digit1, alpha0)),
            char('-').opt(),
            char(';'),
            Vec::new,
            |vec, num| cascade! {vec; ..push(num);},
        )
        .parse(input)
    }

    #[test]
    fn zero_length_item() {
        assert_eq!(
            parse_letters_numbers("----; abc").unwrap(),
            (" abc", vec!["", "", "", "", ""])
        )
    }

    #[test]
    fn zero_length_separators() {
        assert_eq!(
            parse_letters_numbers("abc123abc123; abc").unwrap(),
            (" abc", vec!["abc", "123", "abc", "123"]),
        )
    }

    /// Test that both zero-length separators and items are allowed together,
    /// as long as the loop makes progress
    #[test]
    fn zero_length_mixed() {
        assert_eq!(
            parse_letters_numbers("abc--123abc-123-; abc").unwrap(),
            (" abc", vec!["abc", "", "123", "abc", "123", ""]),
        )
    }

    /// Test that if the loop makes no progress, that's an error
    #[test]
    fn infinite_loop_aborts() {
        let err = parse_letters_numbers("abc123-.; abc").unwrap_err();

        assert_matches!(
            err,
            Err::Error(ErrorTree::Base {
                location: ".; abc",
                kind: BaseErrorKind::Kind(ErrorKind::SeparatedNonEmptyList)
            })
        );
    }

    /// Parse a series of numbers, separated by commas, terminated by optional
    /// comma and eof. Used to test that the terminator "wins" when it and the
    /// separator can match the same string.
    fn parse_comma_separated(input: &str) -> IResult<&str, Vec<i64>, ErrorTree<&str>> {
        parse_separated_terminated(
            parse_from_str(digit1),
            char(','),
            char(',').opt().all_consuming(),
            Vec::new,
            |vec, num| cascade! {vec; ..push(num);},
        )
        .parse(input)
    }

    #[test]
    fn empty_terminator_wins() {
        assert_eq!(
            parse_comma_separated("1,2,3,4").unwrap(),
            ("", vec![1, 2, 3, 4]),
        );
    }

    #[test]
    fn test_terminator_wins() {
        assert_eq!(
            parse_comma_separated("1,2,3,4,").unwrap(),
            ("", vec![1, 2, 3, 4]),
        )
    }
}
