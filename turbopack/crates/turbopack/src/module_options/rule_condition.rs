use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use turbo_esregex::EsRegex;
use turbo_tasks::{primitives::Regex, trace::TraceRawVcs, NonLocalValue, ReadRef, ResolvedVc};
use turbo_tasks_fs::{glob::Glob, FileSystemPath};
use turbopack_core::{
    reference_type::ReferenceType, source::Source, virtual_source::VirtualSource,
};

#[derive(Debug, Clone, Serialize, Deserialize, TraceRawVcs, PartialEq, Eq, NonLocalValue)]
pub enum RuleCondition {
    All(Vec<RuleCondition>),
    Any(Vec<RuleCondition>),
    Not(Box<RuleCondition>),
    ReferenceType(ReferenceType),
    ResourceIsVirtualSource,
    ResourcePathEquals(ReadRef<FileSystemPath>),
    ResourcePathHasNoExtension,
    ResourcePathEndsWith(String),
    ResourcePathInDirectory(String),
    ResourcePathInExactDirectory(ReadRef<FileSystemPath>),
    ContentTypeStartsWith(String),
    ContentTypeEmpty,
    ResourcePathRegex(#[turbo_tasks(trace_ignore)] Regex),
    ResourcePathEsRegex(#[turbo_tasks(trace_ignore)] ReadRef<EsRegex>),
    /// For paths that are within the same filesystem as the `base`, it need to
    /// match the relative path from base to resource. This includes `./` or
    /// `../` prefix. For paths in a different filesystem, it need to match
    /// the resource path in that filesystem without any prefix. This means
    /// any glob starting with `./` or `../` will only match paths in the
    /// project. Globs starting with `**` can match any path.
    ResourcePathGlob {
        base: ReadRef<FileSystemPath>,
        #[turbo_tasks(trace_ignore)]
        glob: ReadRef<Glob>,
    },
    ResourceBasePathGlob(#[turbo_tasks(trace_ignore)] ReadRef<Glob>),
}

impl RuleCondition {
    pub fn all(conditions: Vec<RuleCondition>) -> RuleCondition {
        RuleCondition::All(conditions)
    }

    pub fn any(conditions: Vec<RuleCondition>) -> RuleCondition {
        RuleCondition::Any(conditions)
    }

    #[allow(clippy::should_implement_trait)]
    pub fn not(condition: RuleCondition) -> RuleCondition {
        RuleCondition::Not(Box::new(condition))
    }
}

/// Enum to represent the stack frames for iterative evaluation
enum StackFrame<'a> {
    /// Evaluate the condition
    Evaluate(&'a RuleCondition),
    /// Process all conditions with accumulated result
    All {
        conditions: &'a [RuleCondition],
        index: usize,
    },
    /// Process any conditions with accumulated result
    Any {
        conditions: &'a [RuleCondition],
        index: usize,
    },
    /// Process the negation of a condition
    Not,
}

impl RuleCondition {
    pub async fn matches(
        &self,
        source: ResolvedVc<Box<dyn Source>>,
        path: &FileSystemPath,
        reference_type: &ReferenceType,
    ) -> Result<bool> {
        // Stack for iterative processing
        let mut stack = vec![StackFrame::Evaluate(self)];
        // Results stack to keep track of intermediate results
        let mut results = Vec::new();

        while let Some(frame) = stack.pop() {
            match frame {
                StackFrame::Evaluate(condition) => match condition {
                    RuleCondition::All(conditions) => {
                        if conditions.is_empty() {
                            results.push(true);
                        } else {
                            stack.push(StackFrame::All {
                                conditions,
                                index: 0,
                            });
                        }
                    }
                    RuleCondition::Any(conditions) => {
                        if conditions.is_empty() {
                            results.push(false);
                        } else {
                            stack.push(StackFrame::Any {
                                conditions,
                                index: 0,
                            });
                        }
                    }
                    RuleCondition::Not(inner) => {
                        stack.push(StackFrame::Not);
                        stack.push(StackFrame::Evaluate(inner));
                    }
                    RuleCondition::ResourcePathEquals(other) => {
                        results.push(path == &**other);
                    }
                    RuleCondition::ResourcePathEndsWith(end) => {
                        results.push(path.path.ends_with(end));
                    }
                    RuleCondition::ResourcePathHasNoExtension => {
                        if let Some(i) = path.path.rfind('.') {
                            if let Some(j) = path.path.rfind('/') {
                                results.push(j > i);
                            } else {
                                results.push(false);
                            }
                        } else {
                            results.push(true);
                        }
                    }
                    RuleCondition::ResourcePathInDirectory(dir) => {
                        results.push(
                            path.path.starts_with(&format!("{dir}/"))
                                || path.path.contains(&format!("/{dir}/")),
                        );
                    }
                    RuleCondition::ResourcePathInExactDirectory(parent_path) => {
                        results.push(path.is_inside_ref(parent_path));
                    }
                    RuleCondition::ReferenceType(condition_ty) => {
                        results.push(condition_ty.includes(reference_type));
                    }
                    RuleCondition::ResourceIsVirtualSource => {
                        results
                            .push(ResolvedVc::try_downcast_type::<VirtualSource>(source).is_some());
                    }
                    RuleCondition::ContentTypeStartsWith(start) => {
                        let ident = source.ident().await?;
                        results.push(if let Some(content_type) = ident.content_type.as_ref() {
                            content_type.starts_with(start)
                        } else {
                            false
                        });
                    }
                    RuleCondition::ContentTypeEmpty => {
                        let ident = source.ident().await?;
                        results.push(ident.content_type.is_none());
                    }
                    RuleCondition::ResourcePathGlob { glob, base } => {
                        results.push(if let Some(rel_path) = base.get_relative_path_to(path) {
                            glob.execute(&rel_path)
                        } else {
                            glob.execute(&path.path)
                        });
                    }
                    RuleCondition::ResourceBasePathGlob(glob) => {
                        let basename = path
                            .path
                            .rsplit_once('/')
                            .map_or(path.path.as_str(), |(_, b)| b);
                        results.push(glob.execute(basename));
                    }
                    RuleCondition::ResourcePathRegex(_) => {
                        bail!("ResourcePathRegex not implemented yet");
                    }
                    RuleCondition::ResourcePathEsRegex(regex) => {
                        results.push(regex.is_match(&path.path));
                    }
                },
                StackFrame::All { conditions, index } => {
                    if index >= conditions.len() {
                        // All conditions were true
                        results.push(true);
                    } else if let Some(last_result) = results.pop() {
                        if !last_result {
                            // Short-circuit: if any condition is false, the result is false
                            results.push(false);
                        } else if index < conditions.len() - 1 {
                            // Push back the frame with incremented index
                            stack.push(StackFrame::All {
                                conditions,
                                index: index + 1,
                            });
                            // Evaluate the next condition
                            stack.push(StackFrame::Evaluate(&conditions[index + 1]));
                        } else {
                            // All conditions were true
                            results.push(true);
                        }
                    } else {
                        // First evaluation in the sequence
                        stack.push(StackFrame::All { conditions, index });
                        stack.push(StackFrame::Evaluate(&conditions[index]));
                    }
                }
                StackFrame::Any { conditions, index } => {
                    if index >= conditions.len() {
                        // No condition was true
                        results.push(false);
                    } else if let Some(last_result) = results.pop() {
                        if last_result {
                            // Short-circuit: if any condition is true, the result is true
                            results.push(true);
                        } else if index < conditions.len() - 1 {
                            // Push back the frame with incremented index
                            stack.push(StackFrame::Any {
                                conditions,
                                index: index + 1,
                            });
                            // Evaluate the next condition
                            stack.push(StackFrame::Evaluate(&conditions[index + 1]));
                        } else {
                            // No condition was true
                            results.push(false);
                        }
                    } else {
                        // First evaluation in the sequence
                        stack.push(StackFrame::Any { conditions, index });
                        stack.push(StackFrame::Evaluate(&conditions[index]));
                    }
                }
                StackFrame::Not => {
                    if let Some(result) = results.pop() {
                        results.push(!result);
                    }
                }
            }
        }

        // The final result should be at the top of the results stack
        Ok(results.pop().unwrap_or(false))
    }
}
