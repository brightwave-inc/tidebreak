//! Host access: the delivery reader, the credential-backed GitHub API, and the guarded action API.

use super::*;

#[derive(Clone)]
pub(super) enum ServerDeliveryReader {
    Gh {
        observation: GhObservation,
        runtime: Arc<CodeRuntime>,
    },
    Forge {
        runtime: Arc<CodeRuntime>,
        owner: OwnerId,
    },
}

#[async_trait::async_trait]
impl tidebreak_code_delivery::DeliveryReader for ServerDeliveryReader {
    fn cache_scope(&self) -> &'static str {
        match self {
            Self::Gh { .. } => "gh",
            Self::Forge { .. } => "forge-rest",
        }
    }

    fn validate_pull_request_action(
        &self,
        action: &CodeDeliveryPullRequestAction,
    ) -> Result<(), DeliveryError> {
        if !matches!(self, Self::Forge { .. }) {
            return Ok(());
        }
        match action {
            CodeDeliveryPullRequestAction::MarkReady => Err(DeliveryError::conflict_kind(
                "git_forge_mark_ready_unsupported",
                "This hosted machine cannot mark a draft pull request ready because GitHub's pinned REST API does not expose that transition. Open the pull request on GitHub to mark it ready.",
            )),
            CodeDeliveryPullRequestAction::Merge { admin: true, .. } => {
                Err(DeliveryError::conflict_kind(
                    "git_forge_admin_merge_unsupported",
                    "This hosted machine cannot request an admin branch-protection bypass through GitHub's stable REST API. Open the pull request on GitHub to merge with admin privileges.",
                ))
            }
            _ => Ok(()),
        }
    }

    async fn api(
        &self,
        target: &CodeGitHubRepositoryTarget,
    ) -> Result<tidebreak_code_delivery::DeliveryApiHandle, String> {
        let api = match self {
            Self::Gh {
                observation,
                runtime,
            } => ServerDeliveryApi::Gh {
                observation: observation.clone(),
                host: target.host.clone(),
                search_path: runtime.gh_search_path_owned(),
                runtime: Arc::clone(runtime),
            },
            Self::Forge { runtime, owner } => ServerDeliveryApi::Rest {
                api_base: runtime.forge_api_base_for(&target.host),
                credential: borrow_delivery_credential(runtime, owner, target).await?,
                runtime: Arc::clone(runtime),
            },
        };
        Ok(Arc::new(api))
    }

    async fn action_api(
        &self,
        target: &CodeGitHubRepositoryTarget,
    ) -> Result<tidebreak_code_delivery::DeliveryApiHandle, DeliveryError> {
        self.api(target).await.map_err(|message| match self {
            Self::Forge { .. } => DeliveryError::conflict_kind("git_forge_not_offered", message),
            Self::Gh { .. } => DeliveryError::bad_request_kind("github", message),
        })
    }
}

/// One authenticated Delivery transport. Reads and user actions select the
/// same local `gh` or hosted forge REST path for each repository operation.
pub(super) enum ServerDeliveryApi {
    Gh {
        observation: GhObservation,
        host: String,
        search_path: Option<String>,
        runtime: Arc<CodeRuntime>,
    },
    Rest {
        api_base: String,
        credential: GitCredential,
        runtime: Arc<CodeRuntime>,
    },
}

#[async_trait::async_trait]
impl tidebreak_code_delivery::DeliveryApi for ServerDeliveryApi {
    fn can_mark_pull_request_ready(&self) -> bool {
        matches!(self, Self::Gh { .. })
    }

    async fn get(&self, endpoint: &str) -> Result<Value, String> {
        match self {
            Self::Gh {
                observation, host, ..
            } => {
                run_api_json(
                    observation
                        .binary
                        .as_deref()
                        .expect("authenticated gh has a binary"),
                    host,
                    endpoint,
                )
                .await
            }
            Self::Rest {
                api_base,
                credential,
                ..
            } => crate::code::forge_rest::api_get(api_base, credential, endpoint).await,
        }
    }

    async fn repository(&self, target: &CodeGitHubRepositoryTarget) -> Result<Value, String> {
        match self {
            Self::Gh {
                observation, host, ..
            } => {
                let endpoint = format!("repos/{}/{}", target.owner, target.name);
                run_api_json(
                    observation
                        .binary
                        .as_deref()
                        .expect("authenticated gh has a binary"),
                    host,
                    &endpoint,
                )
                .await
            }
            Self::Rest {
                api_base,
                credential,
                ..
            } => crate::code::forge_rest::repository(api_base, target, credential).await,
        }
    }

    async fn pull_requests(
        &self,
        target: &CodeGitHubRepositoryTarget,
        state: &str,
        fields: &str,
        checks_loaded: bool,
        author: Option<&str>,
    ) -> Result<Vec<Value>, String> {
        match self {
            Self::Gh { observation, .. } => {
                let binary = observation
                    .binary
                    .as_deref()
                    .expect("authenticated gh has a binary");
                let repository = gh::cli_repository(&target.host, &target.owner, &target.name);
                let limit = MAX_REMOTE_ITEMS_PER_REPO.to_string();
                let mut args = vec![
                    "pr",
                    "list",
                    "--repo",
                    repository.as_str(),
                    "--state",
                    state,
                    "--limit",
                    limit.as_str(),
                    "--json",
                    fields,
                ];
                if let Some(author) = author {
                    args.extend(["--author", author]);
                }
                let raw = gh::run_gh(Path::new("."), binary, &args, GH_READ_TIMEOUT).await?;
                let value: Value = serde_json::from_str(&raw)
                    .map_err(|error| format!("could not parse pull requests: {error}"))?;
                Ok(value.as_array().cloned().unwrap_or_default())
            }
            Self::Rest {
                api_base,
                credential,
                ..
            } => {
                crate::code::forge_rest::delivery_pull_requests(
                    api_base,
                    target,
                    credential,
                    state,
                    checks_loaded,
                )
                .await
            }
        }
    }

    async fn deployments(&self, target: &CodeGitHubRepositoryTarget) -> Result<Value, String> {
        match self {
            Self::Gh {
                observation, host, ..
            } => {
                let endpoint = format!(
                    "repos/{}/{}/deployments?per_page=100",
                    target.owner, target.name
                );
                run_api_json(
                    observation
                        .binary
                        .as_deref()
                        .expect("authenticated gh has a binary"),
                    host,
                    &endpoint,
                )
                .await
            }
            Self::Rest {
                api_base,
                credential,
                ..
            } => crate::code::forge_rest::deployments(api_base, target, credential).await,
        }
    }

    async fn workflow_runs(
        &self,
        target: &CodeGitHubRepositoryTarget,
        etag: Option<&str>,
    ) -> Result<
        tidebreak_code_delivery::EndpointRead<Vec<Value>>,
        tidebreak_code_delivery::HostReadError,
    > {
        let read = match self {
            Self::Gh {
                observation,
                runtime,
                ..
            } => {
                let transport = crate::code::pr_fetch::FetchTransport::Gh {
                    cwd: Path::new("."),
                    binary: observation
                        .binary
                        .as_deref()
                        .expect("authenticated gh has a binary"),
                };
                crate::code::pr_fetch::read_workflow_runs(
                    &runtime.host_gate,
                    transport,
                    &target.host,
                    &target.owner,
                    &target.name,
                    etag,
                )
                .await
            }
            Self::Rest {
                api_base,
                credential,
                runtime,
            } => {
                let transport = crate::code::pr_fetch::FetchTransport::Rest {
                    api_base,
                    credential,
                };
                crate::code::pr_fetch::read_workflow_runs(
                    &runtime.host_gate,
                    transport,
                    &target.host,
                    &target.owner,
                    &target.name,
                    etag,
                )
                .await
            }
        };
        match read {
            Ok(crate::code::pr_fetch::EndpointRead::Fresh { value, etag }) => {
                Ok(tidebreak_code_delivery::EndpointRead::Fresh { value, etag })
            }
            Ok(crate::code::pr_fetch::EndpointRead::NotModified) => {
                Ok(tidebreak_code_delivery::EndpointRead::NotModified)
            }
            Ok(crate::code::pr_fetch::EndpointRead::Missing) => {
                Ok(tidebreak_code_delivery::EndpointRead::Missing)
            }
            Err(crate::code::pr_fetch::FetchFailure::Parked(duration)) => {
                Err(tidebreak_code_delivery::HostReadError::Parked(duration))
            }
            Err(error) => Err(tidebreak_code_delivery::HostReadError::Failed(
                error.to_string(),
            )),
        }
    }

    async fn merge_queue_membership(
        &self,
        target: &CodeGitHubRepositoryTarget,
        number: u64,
    ) -> Option<bool> {
        match self {
            Self::Gh {
                observation, host, ..
            } => {
                let binary = observation.binary.as_deref()?;
                let endpoint = format!(
                    "repos/{}/{}/issues/{number}/timeline?per_page=100",
                    target.owner, target.name
                );
                let mut args = vec!["api".to_owned()];
                if host != "github.com" {
                    args.extend(["--hostname".to_owned(), host.clone()]);
                }
                args.extend([
                    endpoint,
                    "--paginate".to_owned(),
                    "--jq".to_owned(),
                    ".[] | select(.event == \"added_to_merge_queue\" or .event == \"removed_from_merge_queue\") | .event".to_owned(),
                ]);
                let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
                let raw = gh::run_gh(Path::new("."), binary, &borrowed, GH_READ_TIMEOUT)
                    .await
                    .ok()?;
                Some(crate::code::pr_fetch::queue_membership_from_events(&raw))
            }
            Self::Rest {
                api_base,
                credential,
                ..
            } => {
                crate::code::forge_rest::merge_queue_state(api_base, target, credential, number)
                    .await
            }
        }
    }

    async fn pull_request(
        &self,
        target: &CodeGitHubRepositoryTarget,
        repository: &CodeGitHubRepositoryRef,
        number: u64,
    ) -> Result<Value, String> {
        match self {
            Self::Gh { observation, .. } => {
                let binary = observation
                    .binary
                    .as_deref()
                    .expect("authenticated gh has a binary");
                let cli_repository =
                    gh::cli_repository(&repository.host, &repository.owner, &repository.name);
                let number = number.to_string();
                let raw = gh::run_gh(
                    Path::new("."),
                    binary,
                    &[
                        "pr",
                        "view",
                        &number,
                        "--repo",
                        &cli_repository,
                        "--json",
                        PR_LIST_FIELDS_WITH_CHECKS,
                    ],
                    GH_READ_TIMEOUT,
                )
                .await?;
                serde_json::from_str(&raw)
                    .map_err(|error| format!("could not parse pull request: {error}"))
            }
            Self::Rest {
                api_base,
                credential,
                ..
            } => {
                crate::code::forge_rest::delivery_pull_request(api_base, target, credential, number)
                    .await
            }
        }
    }

    async fn mark_pull_request_ready(
        &self,
        target: &CodeGitHubRepositoryTarget,
        number: u64,
    ) -> Result<(), DeliveryError> {
        match self {
            Self::Gh { search_path, .. } => gh::mark_pull_request_ready(
                &target.host,
                &target.owner,
                &target.name,
                number,
                search_path.as_deref(),
            )
            .await
            .map_err(map_gh_error),
            Self::Rest { .. } => Err(DeliveryError::conflict_kind(
                "git_forge_mark_ready_unsupported",
                "This hosted machine cannot mark a draft pull request ready because GitHub's pinned REST API does not expose that transition. Open the pull request on GitHub to mark it ready.",
            )),
        }
    }

    async fn merge_pull_request(
        &self,
        target: &CodeGitHubRepositoryTarget,
        number: u64,
        method: CodePrMergeMethod,
        auto: bool,
        admin: bool,
        expected_head_sha: &str,
    ) -> Result<(), DeliveryError> {
        match self {
            Self::Gh { search_path, .. } => gh::merge_pull_request_target(
                &target.host,
                &target.owner,
                &target.name,
                number,
                merge_method(method),
                auto,
                admin,
                expected_head_sha,
                search_path.as_deref(),
            )
            .await
            .map_err(map_gh_error),
            Self::Rest {
                api_base,
                credential,
                ..
            } => {
                if auto {
                    return crate::code::forge_rest::enable_pull_request_auto_merge(
                        api_base,
                        target,
                        credential,
                        number,
                        rest_merge_method(method),
                        expected_head_sha,
                    )
                    .await
                    .map_err(map_forge_action_error);
                }
                if admin {
                    return Err(DeliveryError::conflict_kind(
                        "git_forge_admin_merge_unsupported",
                        "This hosted machine cannot request an admin branch-protection bypass through GitHub's stable REST API. Open the pull request on GitHub to merge with admin privileges.",
                    ));
                }
                crate::code::forge_rest::merge_pull_request(
                    api_base,
                    target,
                    credential,
                    number,
                    rest_merge_method(method),
                    expected_head_sha,
                )
                .await
                .map_err(map_forge_action_error)
            }
        }
    }

    async fn create_stack(
        &self,
        target: &CodeGitHubRepositoryTarget,
        numbers: &[u64],
    ) -> Result<(), DeliveryError> {
        match self {
            Self::Gh {
                host, search_path, ..
            } => gh::create_stack(
                host,
                &target.owner,
                &target.name,
                numbers,
                search_path.as_deref(),
            )
            .await
            .map_err(map_gh_error),
            Self::Rest {
                api_base,
                credential,
                ..
            } => crate::code::forge_rest::create_stack(api_base, target, credential, numbers)
                .await
                .map_err(map_forge_action_error),
        }
    }

    async fn update_pull_request_state(
        &self,
        target: &CodeGitHubRepositoryTarget,
        number: u64,
        state: &str,
    ) -> Result<(), DeliveryError> {
        match (self, state) {
            (Self::Gh { search_path, .. }, "closed") => gh::close_pull_request_target(
                &target.host,
                &target.owner,
                &target.name,
                number,
                search_path.as_deref(),
            )
            .await
            .map_err(map_gh_error),
            (Self::Gh { search_path, .. }, "open") => gh::reopen_pull_request_target(
                &target.host,
                &target.owner,
                &target.name,
                number,
                search_path.as_deref(),
            )
            .await
            .map_err(map_gh_error),
            (Self::Gh { .. }, _) => Err(DeliveryError::internal(
                "Delivery requested an unsupported pull request state",
            )),
            (
                Self::Rest {
                    api_base,
                    credential,
                    ..
                },
                state,
            ) => crate::code::forge_rest::update_pull_request_state(
                api_base, target, credential, number, state,
            )
            .await
            .map_err(map_forge_action_error),
        }
    }

    async fn comment_on_pull_request(
        &self,
        target: &CodeGitHubRepositoryTarget,
        number: u64,
        body: &str,
    ) -> Result<(), DeliveryError> {
        match self {
            Self::Gh { search_path, .. } => gh::comment_on_pull_request_target(
                &target.host,
                &target.owner,
                &target.name,
                number,
                body,
                search_path.as_deref(),
            )
            .await
            .map_err(map_gh_error),
            Self::Rest {
                api_base,
                credential,
                ..
            } => crate::code::forge_rest::comment_on_pull_request(
                api_base, target, credential, number, body,
            )
            .await
            .map_err(map_forge_action_error),
        }
    }

    async fn rerun_failed_jobs(
        &self,
        target: &CodeGitHubRepositoryTarget,
        run_id: u64,
    ) -> Result<(), DeliveryError> {
        match self {
            Self::Gh { observation, .. } => gh::rerun_failed_jobs_with_observation(
                observation,
                &target.host,
                &target.owner,
                &target.name,
                run_id,
            )
            .await
            .map_err(map_gh_error),
            Self::Rest {
                api_base,
                credential,
                ..
            } => crate::code::forge_rest::rerun_failed_jobs(api_base, target, credential, run_id)
                .await
                .map_err(map_forge_action_error),
        }
    }

    async fn rerun_workflow(
        &self,
        target: &CodeGitHubRepositoryTarget,
        run_id: u64,
    ) -> Result<(), DeliveryError> {
        match self {
            Self::Gh { observation, .. } => gh::rerun_workflow_with_observation(
                observation,
                &target.host,
                &target.owner,
                &target.name,
                run_id,
            )
            .await
            .map_err(map_gh_error),
            Self::Rest {
                api_base,
                credential,
                ..
            } => crate::code::forge_rest::rerun_workflow(api_base, target, credential, run_id)
                .await
                .map_err(map_forge_action_error),
        }
    }
}

pub(super) fn github_capability(observation: &GhObservation) -> CodeGitHubCapability {
    CodeGitHubCapability {
        found: observation.found,
        authenticated: observation.authenticated,
        viewer_login: observation.viewer_login.clone(),
        remediation: observation.remediation.clone(),
    }
}

/// Select Delivery's transport for one caller.
///
/// A machine with a gateway lender never consults `gh`: the lender's forge
/// probe is the source of availability and identity. Every other machine
/// keeps the existing GitHub CLI observation unchanged.
pub(super) async fn delivery_access(
    runtime: &Arc<CodeRuntime>,
    owner: &OwnerId,
    force_refresh: bool,
) -> tidebreak_code_delivery::DeliveryAccess {
    if let Some(lender) = runtime.git_credentials() {
        return match lender.git_forge_identity(owner).await {
            Ok(identity) => {
                let viewer_login = match identity.attribution {
                    GitForgeAttribution::Person { login, .. } => Some(login),
                    GitForgeAttribution::Bot { bot_login } => bot_login,
                };
                tidebreak_code_delivery::DeliveryAccess {
                    capability: CodeGitHubCapability {
                        found: true,
                        authenticated: Some(true),
                        viewer_login,
                        remediation: String::new(),
                    },
                    reader: Some(Arc::new(ServerDeliveryReader::Forge {
                        runtime: Arc::clone(runtime),
                        owner: owner.clone(),
                    })),
                    unavailable_kind: "git_forge_not_offered",
                }
            }
            Err(refusal) => tidebreak_code_delivery::DeliveryAccess {
                capability: CodeGitHubCapability {
                    found: !matches!(&refusal, crate::obo_gateway::GitForgeError::NoGitForge),
                    authenticated: Some(false),
                    viewer_login: None,
                    remediation: crate::code::clone::git_forge_refusal_message(&refusal),
                },
                reader: None,
                unavailable_kind: "git_forge_not_offered",
            },
        };
    }

    let search_path = runtime.gh_search_path_owned();
    let observation = if force_refresh {
        gh::refresh_gh_observation(search_path.as_deref()).await
    } else {
        gh::observe_gh(search_path.as_deref()).await
    };
    let unavailable_kind = observation_error_kind(&observation);
    let reader = (observation.authenticated == Some(true)).then(|| {
        Arc::new(ServerDeliveryReader::Gh {
            observation: observation.clone(),
            runtime: Arc::clone(runtime),
        }) as tidebreak_code_delivery::DeliveryReaderHandle
    });
    tidebreak_code_delivery::DeliveryAccess {
        capability: github_capability(&observation),
        reader,
        unavailable_kind,
    }
}

/// Borrow one credential for one repository Delivery operation.
async fn borrow_delivery_credential(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    target: &CodeGitHubRepositoryTarget,
) -> Result<GitCredential, String> {
    if !target
        .host
        .eq_ignore_ascii_case(gh::GIT_CREDENTIAL_FORGE_HOST)
    {
        return Err(format!(
            "this hosted machine can borrow credentials only for {}",
            gh::GIT_CREDENTIAL_FORGE_HOST
        ));
    }
    let lender = runtime
        .git_credentials()
        .ok_or_else(|| "this machine has no hosted forge lender".to_owned())?;
    lender
        .git_credential(owner, &format!("{}/{}", target.owner, target.name))
        .await
        .map_err(|refusal| crate::code::clone::git_forge_refusal_message(&refusal))
}

pub(super) fn merge_method(method: CodePrMergeMethod) -> gh::MergeMethod {
    match method {
        CodePrMergeMethod::Squash => gh::MergeMethod::Squash,
        CodePrMergeMethod::Merge => gh::MergeMethod::Merge,
        CodePrMergeMethod::Rebase => gh::MergeMethod::Rebase,
    }
}

pub(super) fn rest_merge_method(method: CodePrMergeMethod) -> &'static str {
    match method {
        CodePrMergeMethod::Squash => "squash",
        CodePrMergeMethod::Merge => "merge",
        CodePrMergeMethod::Rebase => "rebase",
    }
}
