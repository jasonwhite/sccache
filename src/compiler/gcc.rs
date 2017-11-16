// Copyright 2016 Mozilla Foundation
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use ::compiler::{
    Cacheable,
    CompilerArguments,
};
use compiler::args::*;
use compiler::c::{CCompilerImpl, CCompilerKind, Language, Source, ParsedArguments};
use log::LogLevel::Trace;
use futures::future::Future;
use mock_command::{
    CommandCreatorSync,
    RunCommand,
};
use std::io::Read;
use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process;
use util::{run_input_output, OsStrExt};

use errors::*;

/// A unit struct on which to implement `CCompilerImpl`.
#[derive(Clone, Debug)]
pub struct GCC;

impl CCompilerImpl for GCC {
    fn kind(&self) -> CCompilerKind { CCompilerKind::GCC }
    fn parse_arguments(&self,
                       arguments: &[OsString],
                       cwd: &Path) -> CompilerArguments<ParsedArguments>
    {
        parse_arguments(arguments, cwd, &ARGS[..])
    }

    fn preprocess<T>(&self,
                     input: usize,
                     creator: &T,
                     executable: &Path,
                     parsed_args: &ParsedArguments,
                     cwd: &Path,
                     env_vars: &[(OsString, OsString)])
                     -> SFuture<process::Output> where T: CommandCreatorSync
    {
        preprocess(input, creator, executable, parsed_args, cwd, env_vars)
    }

    fn compile<T>(&self,
                  inputs: &[usize],
                  creator: &T,
                  executable: &Path,
                  parsed_args: &ParsedArguments,
                  cwd: &Path,
                  env_vars: &[(OsString, OsString)])
                  -> SFuture<(Cacheable, process::Output)>
        where T: CommandCreatorSync
    {
        compile(inputs, creator, executable, parsed_args, cwd, env_vars)
    }
}

#[derive(Clone, Debug)]
pub enum GCCArgAttribute {
    TooHard,
    PassThrough,
    PreprocessorArgument,
    DoCompilation,
    Output,
    NeedDepTarget,
    DepTarget,
    Language,
    SplitDwarf,
}

use self::GCCArgAttribute::*;

// Mostly taken from https://github.com/ccache/ccache/blob/master/compopt.c#L32-L84
pub static ARGS: [(ArgInfo, GCCArgAttribute); 60] = [
    flag!("-", TooHard),
    take_arg!("--param", String, Separated, PassThrough),
    flag!("--save-temps", TooHard),
    take_arg!("--serialize-diagnostics", Path, Separated, PassThrough),
    take_arg!("--sysroot", Path, Separated, PassThrough),
    take_arg!("-A", String, Separated, PassThrough),
    take_arg!("-B", Path, CanBeSeparated, PassThrough),
    take_arg!("-D", String, CanBeSeparated, PreprocessorArgument),
    flag!("-E", TooHard),
    take_arg!("-F", Path, CanBeSeparated, PreprocessorArgument),
    take_arg!("-G", String, Separated, PassThrough),
    take_arg!("-I", Path, CanBeSeparated, PreprocessorArgument),
    take_arg!("-L", String, Separated, PassThrough),
    flag!("-M", NeedDepTarget),
    flag!("-MD", NeedDepTarget),
    take_arg!("-MF", Path, Separated, PreprocessorArgument),
    flag!("-MM", NeedDepTarget),
    flag!("-MMD", NeedDepTarget),
    flag!("-MP", NeedDepTarget),
    take_arg!("-MQ", String, Separated, PreprocessorArgument),
    take_arg!("-MT", String, Separated, DepTarget),
    flag!("-P", TooHard),
    take_arg!("-U", String, CanBeSeparated, PreprocessorArgument),
    take_arg!("-V", String, Separated, PassThrough),
    take_arg!("-Xassembler", String, Separated, PassThrough),
    take_arg!("-Xlinker", String, Separated, PassThrough),
    take_arg!("-Xpreprocessor", String, Separated, PreprocessorArgument),
    take_arg!("-arch", String, Separated, PassThrough),
    take_arg!("-aux-info", String, Separated, PassThrough),
    take_arg!("-b", String, Separated, PassThrough),
    flag!("-c", DoCompilation),
    flag!("-fno-working-directory", PreprocessorArgument),
    flag!("-fplugin=libcc1plugin", TooHard),
    flag!("-fprofile-use", TooHard),
    flag!("-frepo", TooHard),
    flag!("-fsyntax-only", TooHard),
    flag!("-fworking-directory", PreprocessorArgument),
    flag!("-gsplit-dwarf", SplitDwarf),
    take_arg!("-idirafter", Path, CanBeSeparated, PreprocessorArgument),
    take_arg!("-iframework", Path, CanBeSeparated, PreprocessorArgument),
    take_arg!("-imacros", Path, CanBeSeparated, PreprocessorArgument),
    take_arg!("-imultilib", Path, CanBeSeparated, PreprocessorArgument),
    take_arg!("-include", Path, CanBeSeparated, PreprocessorArgument),
    take_arg!("-install_name", String, Separated, PassThrough),
    take_arg!("-iprefix", Path, CanBeSeparated, PreprocessorArgument),
    take_arg!("-iquote", Path, CanBeSeparated, PreprocessorArgument),
    take_arg!("-isysroot", Path, CanBeSeparated, PreprocessorArgument),
    take_arg!("-isystem", Path, CanBeSeparated, PreprocessorArgument),
    take_arg!("-iwithprefix", Path, CanBeSeparated, PreprocessorArgument),
    take_arg!("-iwithprefixbefore", Path, CanBeSeparated, PreprocessorArgument),
    flag!("-nostdinc", PreprocessorArgument),
    flag!("-nostdinc++", PreprocessorArgument),
    take_arg!("-o", Path, Separated, Output),
    flag!("-remap", PreprocessorArgument),
    flag!("-save-temps", TooHard),
    take_arg!("-stdlib", String, Concatenated('='), PreprocessorArgument),
    flag!("-trigraphs", PreprocessorArgument),
    take_arg!("-u", String, CanBeSeparated, PassThrough),
    take_arg!("-x", String, CanBeSeparated, Language),
    take_arg!("@", String, Concatenated, TooHard),
];

/// Parse `arguments`, determining whether it is supported.
///
/// If any of the entries in `arguments` result in a compilation that
/// cannot be cached, return `CompilerArguments::CannotCache`.
/// If the commandline described by `arguments` is not compilation,
/// return `CompilerArguments::NotCompilation`.
/// Otherwise, return `CompilerArguments::Ok(ParsedArguments)`, with
/// the `ParsedArguments` struct containing information parsed from
/// `arguments`.
pub fn parse_arguments<S>(
    arguments: &[OsString],
    cwd: &Path,
    arg_info: S,
) -> CompilerArguments<ParsedArguments>
where
    S: SearchableArgInfo<Info = (ArgInfo, GCCArgAttribute)>,
{
    let mut output_arg = None;
    let mut sources = Vec::new();
    let mut dep_target = None;
    let mut common_args = vec!();
    let mut preprocessor_args = vec!();
    let mut compilation = false;
    let mut split_dwarf = false;
    let mut need_explicit_dep_target = false;
    let mut language = None;

    // Custom iterator to expand `@` arguments which stand for reading a file
    // and interpreting it as a list of more arguments.
    let it = ExpandIncludeFile {
        stack: arguments.iter().rev().map(|a| a.to_owned()).collect(),
        cwd: cwd,
    };

    for item in ArgsIter::new(it, arg_info) {
        // Refuse to cache arguments such as "-include@foo" because they're a
        // mess. See https://github.com/mozilla/sccache/issues/150#issuecomment-318586953
        if let Argument::WithValue(_, ref v, ArgDisposition::CanBeSeparated(_)) = item.arg {
            if OsString::from(v.clone()).starts_with("@") {
                return CompilerArguments::CannotCache("@");
            }
        }
        match item.data {
            Some(TooHard) => {
                return CompilerArguments::CannotCache(item.arg.to_str().expect(
                    "Can't be Argument::Raw/UnknownFlag",
                ))
            }
            Some(SplitDwarf) => split_dwarf = true,
            Some(DoCompilation) => compilation = true,
            Some(Output) => output_arg = item.arg.get_value().map(|s| s.unwrap_path()),
            Some(NeedDepTarget) => need_explicit_dep_target = true,
            Some(DepTarget) => dep_target = item.arg.get_value().map(OsString::from),
            Some(PreprocessorArgument) |
            Some(PassThrough) => {}
            Some(Language) => {
                let lang = item.arg.get_value().map(OsString::from);
                let lang = lang.as_ref().map(|a| a.to_string_lossy());
                language = match lang.as_ref().map(|a| a.as_ref()) {
                    Some("c") => Some(Language::C),
                    Some("c++") => Some(Language::Cxx),
                    Some("objective-c") => Some(Language::ObjectiveC),
                    Some("objective-c++") => Some(Language::ObjectiveCxx),
                    _ => return CompilerArguments::CannotCache("-x"),
                };
            }
            None => {
                match item.arg {
                    Argument::Raw(ref val) => {
                        let path = PathBuf::from(val);

                        // Note that `-x <language>` applies to all source files
                        // that follow it. If `-x` was not specified, the
                        // language is deduced based on the source file
                        // extension.
                        let language = match language {
                            Some(l) => l,
                            None => {
                                match Language::from_file_name(&path) {
                                    Some(l) => l,
                                    None => return CompilerArguments::CannotCache("unknown source language"),
                                }
                            }
                        };

                        sources.push(Source::new(path, language));
                    }
                    Argument::UnknownFlag(_) => {}
                    _ => unreachable!(),
                }
            }
        }
        let args = match item.data {
            Some(SplitDwarf) |
            Some(PassThrough) => Some(&mut common_args),
            Some(PreprocessorArgument) |
            Some(NeedDepTarget) => Some(&mut preprocessor_args),
            Some(DoCompilation) |
            Some(Language) |
            Some(Output) |
            Some(DepTarget) => None,
            Some(TooHard) => unreachable!(),
            None => {
                match item.arg {
                    Argument::Raw(_) => None,
                    Argument::UnknownFlag(_) => Some(&mut common_args),
                    _ => unreachable!(),
                }
            }
        };
        if let Some(args) = args {
            // Normalize attributes such as "-I foo", "-D FOO=bar", as
            // "-Ifoo", "-DFOO=bar", etc. and "-includefoo", "idirafterbar" as
            // "-include foo", "-idirafter bar", etc.
            let norm = match item.arg.to_str() {
                Some(s) if s.len() == 2 => NormalizedDisposition::Concatenated,
                _ => NormalizedDisposition::Separated,
            };
            args.extend(item.arg.normalize(norm));
        };
    }

    // We only support compilation.
    if !compilation {
        return CompilerArguments::NotCompilation;
    }

    // We can't cache compilation without an input.
    if sources.is_empty() {
        return CompilerArguments::CannotCache("no input file");
    }

    match output_arg {
        None => {
            // If output file name is not given, use default naming rule for all
            // sources.
            for source in &mut sources {
                source.outputs.insert("obj", source.path.with_extension("o"));
            }
        }
        Some(o) => {
            if sources.len() == 1 {
                sources[0].outputs.insert("obj", PathBuf::from(o));
            } else {
                // An output argument cannot be given if multiple source files
                // are specified. The compiler will fail on this too.
                return CompilerArguments::CannotCache(
                    "multiple inputs with single output");
            }
        }
    }

    if split_dwarf {
        // Each source also gets a ".dwo" file.
        for source in &mut sources {
            source.outputs.insert("dwo", source.path.with_extension("dwo"));
        }
    }

    if need_explicit_dep_target {
        if sources.len() == 1 {
            preprocessor_args.push("-MT".into());
            preprocessor_args.push(
                dep_target.unwrap_or(sources[0].outputs["obj"]
                                               .clone()
                                               .into_os_string()));
        } else {
            return CompilerArguments::CannotCache("makefile deps with multiple inputs");
        }
    }

    CompilerArguments::Ok(ParsedArguments {
        sources: sources,
        depfile: None,
        preprocessor_args: preprocessor_args,
        common_args: common_args,
        msvc_show_includes: false,
    })
}

pub fn preprocess<T>(input: usize,
                     creator: &T,
                     executable: &Path,
                     parsed_args: &ParsedArguments,
                     cwd: &Path,
                     env_vars: &[(OsString, OsString)])
                     -> SFuture<process::Output>
    where T: CommandCreatorSync
{
    trace!("preprocess");
    let language = match parsed_args.sources[0].language {
        Language::C => "c",
        Language::Cxx => "c++",
        Language::ObjectiveC => "objective-c",
        Language::ObjectiveCxx => "objective-c++",
    };
    let mut cmd = creator.clone().new_command_sync(executable);
    cmd.arg("-x").arg(language)
        .arg("-E")
        .arg(&parsed_args.sources[input].path)
        .args(&parsed_args.preprocessor_args)
        .args(&parsed_args.common_args)
        .env_clear()
        .envs(env_vars.iter().map(|&(ref k, ref v)| (k, v)))
        .current_dir(cwd);

    if log_enabled!(Trace) {
        trace!("preprocess: {:?}", cmd);
    }
    run_input_output(cmd, None)
}

pub fn compile<T>(inputs: &[usize],
              creator: &T,
              executable: &Path,
              parsed_args: &ParsedArguments,
              cwd: &Path,
              env_vars: &[(OsString, OsString)])
              -> SFuture<(Cacheable, process::Output)>
    where T: CommandCreatorSync
{
    trace!("compile");

    let out_file = match parsed_args.sources[0].outputs.get("obj") {
        Some(obj) => obj,
        None => {
            return f_err("Missing object file output")
        }
    };

    // Pass the language explicitly as we might have gotten it from the
    // command line.
    let language = match parsed_args.sources[0].language {
        Language::C => "c",
        Language::Cxx => "c++",
        Language::ObjectiveC => "objective-c",
        Language::ObjectiveCxx => "objective-c++",
    };
    let mut attempt = creator.clone().new_command_sync(executable);
    attempt.arg("-x").arg(language)
        .arg("-c")
        .arg(&parsed_args.sources[0].path)
        .arg("-o").arg(&out_file)
        .args(&parsed_args.preprocessor_args)
        .args(&parsed_args.common_args)
        .env_clear()
        .envs(env_vars.iter().map(|&(ref k, ref v)| (k, v)))
        .current_dir(&cwd);
    Box::new(run_input_output(attempt, None).map(|output| {
        (Cacheable::Yes, output)
    }))
}

struct ExpandIncludeFile<'a> {
    cwd: &'a Path,
    stack: Vec<OsString>,
}

impl<'a> Iterator for ExpandIncludeFile<'a> {
    type Item = OsString;

    fn next(&mut self) -> Option<OsString> {
        loop {
            let arg = match self.stack.pop() {
                Some(arg) => arg,
                None => return None,
            };
            let file = match arg.split_prefix("@") {
                Some(arg) => self.cwd.join(&arg),
                None => return Some(arg),
            };

            // According to gcc [1], @file means:
            //
            //     Read command-line options from file. The options read are
            //     inserted in place of the original @file option. If file does
            //     not exist, or cannot be read, then the option will be
            //     treated literally, and not removed.
            //
            //     Options in file are separated by whitespace. A
            //     whitespace character may be included in an option by
            //     surrounding the entire option in either single or double
            //     quotes. Any character (including a backslash) may be
            //     included by prefixing the character to be included with
            //     a backslash. The file may itself contain additional
            //     @file options; any such options will be processed
            //     recursively.
            //
            // So here we interpret any I/O errors as "just return this
            // argument". Currently we don't implement handling of arguments
            // with quotes, so if those are encountered we just pass the option
            // through literally anyway.
            //
            // At this time we interpret all `@` arguments above as non
            // cacheable, so if we fail to interpret this we'll just call the
            // compiler anyway.
            //
            // [1]: https://gcc.gnu.org/onlinedocs/gcc/Overall-Options.html#Overall-Options
            let mut contents = String::new();
            let res = File::open(&file).and_then(|mut f| {
                f.read_to_string(&mut contents)
            });
            if let Err(e) = res {
                debug!("failed to read @-file `{}`: {}", file.display(), e);
                return Some(arg)
            }
            if contents.contains('"') || contents.contains('\'') {
                return Some(arg)
            }
            let new_args = contents.split_whitespace().collect::<Vec<_>>();
            self.stack.extend(new_args.iter().rev().map(|s| s.into()));
        }
    }
}

#[cfg(test)]
mod test {
    use std::fs::File;
    use std::io::Write;

    use super::*;
    use ::compiler::*;
    use mock_command::*;
    use test::utils::*;
    use tempdir::TempDir;

    fn _parse_arguments(arguments: &[String]) -> CompilerArguments<ParsedArguments> {
        let args = arguments.iter().map(OsString::from).collect::<Vec<_>>();
        parse_arguments(&args, ".".as_ref(), &ARGS[..])
    }

    #[test]
    fn test_parse_arguments_simple() {
        let args = stringvec!["-c", "foo.c", "-o", "foo.o"];
        let ParsedArguments {
            sources,
            depfile: _,
            preprocessor_args,
            msvc_show_includes,
            common_args,
        } = match _parse_arguments(&args) {
            CompilerArguments::Ok(args) => args,
            o @ _ => panic!("Got unexpected parse result: {:?}", o),
        };
        assert!(true, "Parsed ok");
        assert_eq!(Some("foo.c"), sources[0].path.to_str());
        assert_eq!(Language::C, sources[0].language);
        assert_map_contains!(sources[0].outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, sources[0].outputs.len());
        assert!(preprocessor_args.is_empty());
        assert!(common_args.is_empty());
        assert!(!msvc_show_includes);
    }

    #[test]
    fn test_parse_arguments_default_name() {
        let args = stringvec!["-c", "foo.c"];
        let ParsedArguments {
            sources,
            depfile: _,
            preprocessor_args,
            msvc_show_includes,
            common_args,
        } = match _parse_arguments(&args) {
            CompilerArguments::Ok(args) => args,
            o @ _ => panic!("Got unexpected parse result: {:?}", o),
        };
        assert!(true, "Parsed ok");
        assert_eq!(Some("foo.c"), sources[0].path.to_str());
        assert_eq!(Language::C, sources[0].language);
        assert_map_contains!(sources[0].outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, sources[0].outputs.len());
        assert!(preprocessor_args.is_empty());
        assert!(common_args.is_empty());
        assert!(!msvc_show_includes);
    }

    #[test]
    fn test_parse_arguments_split_dwarf() {
        let args = stringvec!["-gsplit-dwarf", "-c", "foo.cpp", "-o", "foo.o"];
        let ParsedArguments {
            sources,
            depfile: _,
            preprocessor_args,
            msvc_show_includes,
            common_args,
        } = match _parse_arguments(&args) {
            CompilerArguments::Ok(args) => args,
            o @ _ => panic!("Got unexpected parse result: {:?}", o),
        };
        assert!(true, "Parsed ok");
        assert_eq!(Some("foo.cpp"), sources[0].path.to_str());
        assert_eq!(Language::Cxx, sources[0].language);
        assert_map_contains!(sources[0].outputs,
                             ("obj", PathBuf::from("foo.o")),
                             ("dwo", PathBuf::from("foo.dwo")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(2, sources[0].outputs.len());
        assert!(preprocessor_args.is_empty());
        assert_eq!(ovec!["-gsplit-dwarf"], common_args);
        assert!(!msvc_show_includes);
    }

    #[test]
    fn test_parse_arguments_extra() {
        let args = stringvec!["-c", "foo.cc", "-fabc", "-o", "foo.o", "-mxyz"];
        let ParsedArguments {
            sources,
            depfile: _,
            preprocessor_args,
            msvc_show_includes,
            common_args,
        } = match _parse_arguments(&args) {
            CompilerArguments::Ok(args) => args,
            o @ _ => panic!("Got unexpected parse result: {:?}", o),
        };
        assert!(true, "Parsed ok");
        assert_eq!(Some("foo.cc"), sources[0].path.to_str());
        assert_eq!(Language::Cxx, sources[0].language);
        assert_map_contains!(sources[0].outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, sources[0].outputs.len());
        assert!(preprocessor_args.is_empty());
        assert_eq!(ovec!["-fabc", "-mxyz"], common_args);
        assert!(!msvc_show_includes);
    }

    #[test]
    fn test_parse_arguments_values() {
        let args = stringvec!["-c", "foo.cxx", "-fabc", "-I", "include", "-o", "foo.o", "-include", "file"];
        let ParsedArguments {
            sources,
            depfile: _,
            preprocessor_args,
            msvc_show_includes,
            common_args,
        } = match _parse_arguments(&args) {
            CompilerArguments::Ok(args) => args,
            o @ _ => panic!("Got unexpected parse result: {:?}", o),
        };
        assert!(true, "Parsed ok");
        assert_eq!(Some("foo.cxx"), sources[0].path.to_str());
        assert_eq!(Language::Cxx, sources[0].language);
        assert_map_contains!(sources[0].outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, sources[0].outputs.len());
        assert_eq!(ovec!["-Iinclude", "-include", "file"], preprocessor_args);
        assert_eq!(ovec!["-fabc"], common_args);
        assert!(!msvc_show_includes);
    }

    #[test]
    fn test_parse_arguments_preprocessor_args() {
        let args = stringvec!["-c", "foo.c", "-fabc", "-MF", "file", "-o", "foo.o", "-MQ", "abc"];
        let ParsedArguments {
            sources,
            depfile: _,
            preprocessor_args,
            msvc_show_includes,
            common_args,
        } = match _parse_arguments(&args) {
            CompilerArguments::Ok(args) => args,
            o @ _ => panic!("Got unexpected parse result: {:?}", o),
        };
        assert!(true, "Parsed ok");
        assert_eq!(Some("foo.c"), sources[0].path.to_str());
        assert_eq!(Language::C, sources[0].language);
        assert_map_contains!(sources[0].outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, sources[0].outputs.len());
        assert_eq!(ovec!["-MF", "file", "-MQ", "abc"], preprocessor_args);
        assert_eq!(ovec!["-fabc"], common_args);
        assert!(!msvc_show_includes);
    }

    #[test]
    fn test_parse_arguments_explicit_dep_target() {
        let args = stringvec!["-c", "foo.c", "-MT", "depfile", "-fabc", "-MF", "file", "-o", "foo.o"];
        let ParsedArguments {
            sources,
            depfile: _,
            preprocessor_args,
            msvc_show_includes,
            common_args,
        } = match _parse_arguments(&args) {
            CompilerArguments::Ok(args) => args,
            o @ _ => panic!("Got unexpected parse result: {:?}", o),
        };
        assert!(true, "Parsed ok");
        assert_eq!(Some("foo.c"), sources[0].path.to_str());
        assert_eq!(Language::C, sources[0].language);
        assert_map_contains!(sources[0].outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, sources[0].outputs.len());
        assert_eq!(ovec!["-MF", "file"], preprocessor_args);
        assert_eq!(ovec!["-fabc"], common_args);
        assert!(!msvc_show_includes);
    }

    #[test]
    fn test_parse_arguments_explicit_dep_target_needed() {
        let args = stringvec!["-c", "foo.c", "-MT", "depfile", "-fabc", "-MF", "file", "-o", "foo.o", "-MD"];
        let ParsedArguments {
            sources,
            depfile: _,
            preprocessor_args,
            msvc_show_includes,
            common_args,
        } = match _parse_arguments(&args) {
            CompilerArguments::Ok(args) => args,
            o @ _ => panic!("Got unexpected parse result: {:?}", o),
        };
        assert!(true, "Parsed ok");
        assert_eq!(Some("foo.c"), sources[0].path.to_str());
        assert_eq!(Language::C, sources[0].language);
        assert_map_contains!(sources[0].outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, sources[0].outputs.len());
        assert_eq!(ovec!["-MF", "file", "-MD", "-MT", "depfile"], preprocessor_args);
        assert_eq!(ovec!["-fabc"], common_args);
        assert!(!msvc_show_includes);
    }

    #[test]
    fn test_parse_arguments_dep_target_needed() {
        let args = stringvec!["-c", "foo.c", "-fabc", "-MF", "file", "-o", "foo.o", "-MD"];
        let ParsedArguments {
            sources,
            depfile: _,
            preprocessor_args,
            msvc_show_includes,
            common_args,
        } = match _parse_arguments(&args) {
            CompilerArguments::Ok(args) => args,
            o @ _ => panic!("Got unexpected parse result: {:?}", o),
        };
        assert!(true, "Parsed ok");
        assert_eq!(Some("foo.c"), sources[0].path.to_str());
        assert_eq!(Language::C, sources[0].language);
        assert_map_contains!(sources[0].outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, sources[0].outputs.len());
        assert_eq!(ovec!["-MF", "file", "-MD", "-MT", "foo.o"], preprocessor_args);
        assert_eq!(ovec!["-fabc"], common_args);
        assert!(!msvc_show_includes);
    }

    #[test]
    fn test_parse_arguments_empty_args() {
        assert_eq!(CompilerArguments::NotCompilation,
                   _parse_arguments(&vec!()));
    }

    #[test]
    fn test_parse_arguments_not_compile() {
        assert_eq!(CompilerArguments::NotCompilation,
                   _parse_arguments(&stringvec!["-o", "foo"]));
    }

    #[test]
    fn test_parse_arguments_many_inputs() {
        let args = stringvec!["-c", "-gsplit-dwarf", "foo.c", "bar.cpp"];
        let ParsedArguments {
            sources,
            depfile: _,
            preprocessor_args,
            msvc_show_includes,
            common_args,
        } = match _parse_arguments(&args) {
            CompilerArguments::Ok(args) => args,
            o @ _ => panic!("Got unexpected parse result: {:?}", o),
        };
        assert!(true, "Parsed ok");
        assert_eq!(
            sources,
            vec![
                Source {
                    path: PathBuf::from("foo.c"),
                    language: Language::C,
                    outputs: vec![
                        ("obj", PathBuf::from("foo.o")),
                        ("dwo", PathBuf::from("foo.dwo")),
                    ].into_iter().collect()
                },
                Source {
                    path: PathBuf::from("bar.cpp"),
                    language: Language::Cxx,
                    outputs: vec![
                        ("obj", PathBuf::from("bar.o")),
                        ("dwo", PathBuf::from("bar.dwo")),
                    ].into_iter().collect()
                },
            ]
        );
        assert!(preprocessor_args.is_empty());
        assert_eq!(ovec!["-gsplit-dwarf"], common_args);
        assert!(!msvc_show_includes);
    }

    #[test]
    fn test_parse_arguments_link() {
        assert_eq!(CompilerArguments::CannotCache("unknown source language"),
                   _parse_arguments(&stringvec!["-shared", "foo.o", "-o", "foo.so", "bar.o"]));
    }

    #[test]
    fn test_parse_arguments_pgo() {
        assert_eq!(CompilerArguments::CannotCache("-fprofile-use"),
                   _parse_arguments(&stringvec!["-c", "foo.c", "-fprofile-use", "-o", "foo.o"]));
    }

    #[test]
    fn test_parse_arguments_response_file() {
        assert_eq!(CompilerArguments::CannotCache("@"),
                   _parse_arguments(&stringvec!["-c", "foo.c", "@foo", "-o", "foo.o"]));
    }

    #[test]
    fn at_signs() {
        let td = TempDir::new("sccache").unwrap();
        File::create(td.path().join("foo")).unwrap().write_all(b"\
            -c foo.c -o foo.o\
        ").unwrap();
        let arg = format!("@{}", td.path().join("foo").display());
        let ParsedArguments {
            sources,
            depfile: _,
            preprocessor_args,
            msvc_show_includes,
            common_args,
        } = match _parse_arguments(&[arg]) {
            CompilerArguments::Ok(args) => args,
            o @ _ => panic!("Got unexpected parse result: {:?}", o),
        };
        assert!(true, "Parsed ok");
        assert_eq!(Some("foo.c"), sources[0].path.to_str());
        assert_eq!(Language::C, sources[0].language);
        assert_map_contains!(sources[0].outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, sources[0].outputs.len());
        assert!(preprocessor_args.is_empty());
        assert!(common_args.is_empty());
        assert!(!msvc_show_includes);
    }

    #[test]
    fn test_compile_simple() {
        let creator = new_creator();
        let f = TestFixture::new();
        let parsed_args = ParsedArguments {
            sources: vec![Source {
                path: "foo.c".into(),
                language: Language::C,
                outputs: vec![("obj", "foo.o".into())].into_iter().collect(),
            }],
            depfile: None,
            preprocessor_args: vec!(),
            common_args: vec!(),
            msvc_show_includes: false,
        };
        let compiler = &f.bins[0];
        // Compiler invocation.
        next_command(&creator, Ok(MockChild::new(exit_status(0), "", "")));
        let (cacheable, _) = compile(&creator,
                                     &compiler,
                                     &parsed_args,
                                     f.tempdir.path(),
                                     &[]).wait().unwrap();
        assert_eq!(Cacheable::Yes, cacheable);
        // Ensure that we ran all processes.
        assert_eq!(0, creator.lock().unwrap().children.len());
    }
}
