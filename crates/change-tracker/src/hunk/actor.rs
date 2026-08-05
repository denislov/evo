use super::*;

impl ActorState {
    pub(super) fn new(root: PathBuf, options: HunkTrackerOptions) -> Self {
        Self {
            root,
            options,
            files: BTreeMap::new(),
            pending_receipts: VecDeque::new(),
            pending_events: VecDeque::new(),
            facts: VecDeque::new(),
            next_hunk: 1,
            next_fact: 1,
            history_bytes: 0,
            reconcile: ReconcileState::Ready,
        }
    }

    pub(super) fn record_receipt(
        &mut self,
        receipt: ChangeReceipt,
        source: ChangeSource,
        context: TrackingContext,
    ) -> Result<(), ChangeTrackerError> {
        self.flush_expired()?;
        if !source.accepts_receipt() {
            return Err(ChangeTrackerError::InvalidFact {
                message: format!("{source:?} cannot be submitted as a mutation receipt"),
            });
        }
        validate_context(&context)?;
        validate_revision(&receipt.after_revision, "after_revision")?;
        if let Some(before) = receipt.before_revision.as_deref() {
            validate_revision(before, "before_revision")?;
        }
        if receipt.target_fingerprint.is_empty() || receipt.origin.is_empty() {
            return Err(ChangeTrackerError::InvalidFact {
                message: "receipt requires target_fingerprint and origin".into(),
            });
        }
        if receipt
            .unified_diff
            .as_ref()
            .is_some_and(|diff| diff.len() > self.options.max_diff_bytes)
        {
            return Err(ChangeTrackerError::BudgetExceeded {
                message: format!("receipt diff exceeds {} bytes", self.options.max_diff_bytes),
            });
        }
        let path = normalize_relative(Path::new(&receipt.path))?;
        self.ensure_file_budget(&path)?;
        self.ensure_fact_budget()?;

        let matching_event = self.pending_events.iter().position(|pending| {
            pending.event.path == path
                && pending.observed.exists == receipt.after_exists
                && pending.observed.revision == receipt.after_revision
        });
        if matching_event.is_none() && self.pending_receipts.len() >= self.options.max_pending_facts
        {
            return Err(ChangeTrackerError::BudgetExceeded {
                message: "pending receipt budget exhausted".into(),
            });
        }

        let observed = read_observed(&self.root, &path, self.options.max_content_bytes)?;
        if observed.exists != receipt.after_exists || observed.revision != receipt.after_revision {
            return Err(ChangeTrackerError::InvalidFact {
                message: format!(
                    "receipt no longer matches workspace state: {}",
                    path.display()
                ),
            });
        }
        let current = FileVersion::from_observed(observed);
        if let Some(previous) = self
            .files
            .get(&path)
            .and_then(|state| state.current.as_ref())
        {
            let before_matches = match receipt.before_revision.as_deref() {
                Some(revision) => previous.exists && previous.revision == revision,
                None => !previous.exists,
            };
            if !before_matches {
                return Err(ChangeTrackerError::InvalidFact {
                    message: format!("receipt before_revision is stale: {}", path.display()),
                });
            }
        }
        let previous_state = self.files.get(&path).cloned();
        if !self.files.contains_key(&path) {
            let baseline = baseline_from_receipt(&receipt, &current)?;
            self.files.entry(path.clone()).or_default().baseline = Some(baseline);
        }
        {
            let state = self.files.entry(path.clone()).or_default();
            state.current = Some(current);
            state.target_fingerprint = (receipt.before_revision.is_some() == receipt.after_exists)
                .then(|| receipt.target_fingerprint.clone());
            state.mutation_kind = receipt.origin.clone();
            state.agent_touched |= source == ChangeSource::AgentEdit;
        }
        if let Err(error) = self.recompute_and_record(
            path.clone(),
            source,
            Some(context),
            receipt.before_revision.clone(),
        ) {
            match previous_state {
                Some(state) => {
                    self.files.insert(path, state);
                }
                None => {
                    self.files.remove(&path);
                }
            }
            return Err(error);
        }

        if let Some(index) = matching_event {
            self.pending_events.remove(index);
        } else {
            self.pending_receipts.push_back(PendingReceipt {
                path,
                after_revision: receipt.after_revision,
                after_exists: receipt.after_exists,
                expires: Instant::now() + self.options.causal_window,
            });
        }
        Ok(())
    }

    pub(super) fn observe(&mut self, event: FsEvent) -> Result<(), ChangeTrackerError> {
        self.flush_expired()?;
        match event {
            FsEvent::Git(_) => Ok(()),
            FsEvent::WatchGap { lost } => {
                self.reconcile = ReconcileState::Required {
                    lost: match self.reconcile {
                        ReconcileState::Ready => lost,
                        ReconcileState::Required { lost: previous } => {
                            previous.saturating_add(lost)
                        }
                    },
                };
                self.pending_events.clear();
                self.pending_receipts.clear();
                Ok(())
            }
            FsEvent::Workspace(event) => self.observe_workspace(event),
        }
    }

    pub(super) fn accept_hunk(
        &mut self,
        path: PathBuf,
        expected_sequence: u64,
        hunk_id: HunkId,
        expected_revision: String,
        expected_target_fingerprint: String,
    ) -> Result<(), ChangeTrackerError> {
        self.flush_expired()?;
        let path = normalize_relative(&path)?;
        self.validate_disk_revision(&path, expected_sequence, &expected_revision)?;
        self.validate_target_fingerprint(&path, &expected_target_fingerprint)?;
        let (hunk, baseline, current) = {
            let state = self
                .files
                .get(&path)
                .ok_or_else(|| ChangeTrackerError::InvalidFact {
                    message: format!("review file does not exist: {}", path.display()),
                })?;
            let snapshot =
                state
                    .snapshot
                    .as_ref()
                    .ok_or_else(|| ChangeTrackerError::InvalidFact {
                        message: format!("review file has no active snapshot: {}", path.display()),
                    })?;
            let hunk = snapshot
                .hunks
                .iter()
                .find(|hunk| hunk.id == hunk_id)
                .cloned()
                .ok_or_else(|| ChangeTrackerError::InvalidFact {
                    message: format!(
                        "review hunk identity is no longer active: {}",
                        hunk_id.as_str()
                    ),
                })?;
            let baseline = state.baseline.clone().ok_or_else(unpatchable_review)?;
            let current = state.current.clone().ok_or_else(unpatchable_review)?;
            (hunk, baseline, current)
        };
        let accepted = if hunk.unified_diff.is_none() && baseline.content == current.content {
            current.clone()
        } else {
            let baseline_content = baseline.content.as_deref().ok_or_else(unpatchable_review)?;
            let current_content = current.content.as_deref().ok_or_else(unpatchable_review)?;
            let content = replace_line_range(
                baseline_content,
                hunk.range.old_start,
                hunk.range.old_lines,
                current_content,
                hunk.range.new_start,
                hunk.range.new_lines,
            )
            .map_err(|message| ChangeTrackerError::InvalidFact { message })?;
            let exists = if !baseline.exists && current.exists {
                true
            } else if baseline.exists && !current.exists && content.is_empty() {
                false
            } else {
                baseline.exists
            };
            FileVersion {
                exists,
                revision: revision(&content),
                content: Some(content),
            }
        };
        let previous_baseline = self
            .files
            .get(&path)
            .and_then(|state| state.baseline.clone());
        self.files
            .get_mut(&path)
            .expect("validated review state remains owned by actor")
            .baseline = Some(accepted);
        if let Err(error) = self.recompute_and_record(
            path.clone(),
            hunk.source,
            hunk.context,
            hunk.before_revision,
        ) {
            self.files
                .get_mut(&path)
                .expect("review state remains owned after failed accept")
                .baseline = previous_baseline;
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn accept_file(
        &mut self,
        path: PathBuf,
        expected_sequence: u64,
        expected_revision: String,
        expected_target_fingerprint: String,
    ) -> Result<(), ChangeTrackerError> {
        self.flush_expired()?;
        let path = normalize_relative(&path)?;
        self.validate_disk_revision(&path, expected_sequence, &expected_revision)?;
        self.validate_target_fingerprint(&path, &expected_target_fingerprint)?;
        let state = self
            .files
            .get_mut(&path)
            .expect("validated review state remains owned by actor");
        state.baseline = state.current.clone();
        state.snapshot = None;
        state.identities.clear();
        Ok(())
    }

    pub(super) fn prepare_reject_hunk(
        &mut self,
        path: PathBuf,
        expected_sequence: u64,
        hunk_id: HunkId,
        expected_revision: String,
        expected_target_fingerprint: String,
    ) -> Result<RejectPlan, ChangeTrackerError> {
        self.flush_expired()?;
        let path = normalize_relative(&path)?;
        self.validate_disk_revision(&path, expected_sequence, &expected_revision)?;
        let (hunk, baseline, current, target_fingerprint) =
            self.reject_inputs(&path, Some(&hunk_id), &expected_target_fingerprint)?;
        let replacement = if hunk.unified_diff.is_none() && baseline.content == current.content {
            replacement_for_version(&baseline)?
        } else {
            let baseline_content = baseline.content.as_deref().ok_or_else(unpatchable_review)?;
            let current_content = current.content.as_deref().ok_or_else(unpatchable_review)?;
            let content = replace_line_range(
                current_content,
                hunk.range.new_start,
                hunk.range.new_lines,
                baseline_content,
                hunk.range.old_start,
                hunk.range.old_lines,
            )
            .map_err(|message| ChangeTrackerError::InvalidFact { message })?;
            if !baseline.exists && current.exists && content.is_empty() {
                RejectReplacement::Delete
            } else {
                RejectReplacement::Write(content)
            }
        };
        Ok(RejectPlan {
            path,
            expected_sequence,
            expected_revision,
            expected_exists: current.exists,
            target_fingerprint,
            replacement,
        })
    }

    pub(super) fn prepare_reject_file(
        &mut self,
        path: PathBuf,
        expected_sequence: u64,
        expected_revision: String,
        expected_target_fingerprint: String,
    ) -> Result<RejectPlan, ChangeTrackerError> {
        self.flush_expired()?;
        let path = normalize_relative(&path)?;
        self.validate_disk_revision(&path, expected_sequence, &expected_revision)?;
        let (_, baseline, current, target_fingerprint) =
            self.reject_inputs(&path, None, &expected_target_fingerprint)?;
        Ok(RejectPlan {
            path,
            expected_sequence,
            expected_revision,
            expected_exists: current.exists,
            target_fingerprint,
            replacement: replacement_for_version(&baseline)?,
        })
    }

    fn reject_inputs(
        &self,
        path: &Path,
        hunk_id: Option<&HunkId>,
        expected_target_fingerprint: &str,
    ) -> Result<(HunkSnapshot, FileVersion, FileVersion, String), ChangeTrackerError> {
        if expected_target_fingerprint.is_empty() {
            return Err(ChangeTrackerError::InvalidFact {
                message: "review action requires a target fingerprint".into(),
            });
        }
        let state = self.files.get(path).ok_or_else(unpatchable_review)?;
        if state
            .target_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| fingerprint != expected_target_fingerprint)
        {
            return Err(ChangeTrackerError::InvalidFact {
                message: format!("review target fingerprint is stale: {}", path.display()),
            });
        }
        let snapshot = state.snapshot.as_ref().ok_or_else(unpatchable_review)?;
        let hunk = match hunk_id {
            Some(hunk_id) => snapshot
                .hunks
                .iter()
                .find(|hunk| &hunk.id == hunk_id)
                .cloned()
                .ok_or_else(|| ChangeTrackerError::InvalidFact {
                    message: format!(
                        "review hunk identity is no longer active: {}",
                        hunk_id.as_str()
                    ),
                })?,
            None => snapshot
                .hunks
                .first()
                .cloned()
                .ok_or_else(unpatchable_review)?,
        };
        Ok((
            hunk,
            state.baseline.clone().ok_or_else(unpatchable_review)?,
            state.current.clone().ok_or_else(unpatchable_review)?,
            expected_target_fingerprint.to_owned(),
        ))
    }

    fn validate_disk_revision(
        &self,
        path: &Path,
        expected_sequence: u64,
        expected_revision: &str,
    ) -> Result<(), ChangeTrackerError> {
        let snapshot = self
            .files
            .get(path)
            .and_then(|state| state.snapshot.as_ref())
            .ok_or_else(|| ChangeTrackerError::InvalidFact {
                message: format!("review file does not exist: {}", path.display()),
            })?;
        if snapshot.recorded_sequence != expected_sequence
            || snapshot.after_revision != expected_revision
        {
            return Err(ChangeTrackerError::InvalidFact {
                message: format!("stale review target: {}", path.display()),
            });
        }
        let observed = read_observed(&self.root, path, self.options.max_content_bytes)?;
        if observed.exists != snapshot.after_exists || observed.revision != snapshot.after_revision
        {
            return Err(ChangeTrackerError::InvalidFact {
                message: format!(
                    "workspace changed after review snapshot: {}",
                    path.display()
                ),
            });
        }
        Ok(())
    }

    fn validate_target_fingerprint(
        &self,
        path: &Path,
        expected_target_fingerprint: &str,
    ) -> Result<(), ChangeTrackerError> {
        if expected_target_fingerprint.is_empty() {
            return Err(ChangeTrackerError::InvalidFact {
                message: "review action requires a target fingerprint".into(),
            });
        }
        if self
            .files
            .get(path)
            .and_then(|state| state.target_fingerprint.as_deref())
            .is_some_and(|fingerprint| fingerprint != expected_target_fingerprint)
        {
            return Err(ChangeTrackerError::InvalidFact {
                message: format!("review target fingerprint is stale: {}", path.display()),
            });
        }
        Ok(())
    }

    fn observe_workspace(&mut self, event: SemanticEvent) -> Result<(), ChangeTrackerError> {
        if event.root != self.root {
            return Err(ChangeTrackerError::InvalidFact {
                message: format!(
                    "event root {} does not match tracker root {}",
                    event.root.display(),
                    self.root.display()
                ),
            });
        }
        let path = normalize_relative(&event.path)?;
        if event.kind == FsChangeKind::Renamed {
            let from = event
                .from
                .as_deref()
                .ok_or_else(|| ChangeTrackerError::InvalidFact {
                    message: "rename event is missing its source path".into(),
                })?;
            let from = normalize_relative(from)?;
            if let Some(mut state) = self.files.remove(&from) {
                if self.files.contains_key(&path) {
                    return Err(ChangeTrackerError::InvalidFact {
                        message: format!(
                            "rename destination already has tracked state: {}",
                            path.display()
                        ),
                    });
                }
                self.ensure_file_budget(&path)?;
                if let Some(snapshot) = &mut state.snapshot {
                    snapshot.path = path.clone();
                }
                self.files.insert(path.clone(), state);
            }
            for receipt in &mut self.pending_receipts {
                if receipt.path == from {
                    receipt.path = path.clone();
                }
            }
            for pending in &mut self.pending_events {
                if pending.event.path == from {
                    pending.event.path = path.clone();
                }
            }
        }
        self.ensure_file_budget(&path)?;
        let observed = read_observed(&self.root, &path, self.options.max_content_bytes)?;
        if let Some(index) = self.pending_receipts.iter().position(|pending| {
            pending.path == path
                && pending.after_exists == observed.exists
                && pending.after_revision == observed.revision
        }) {
            self.pending_receipts.remove(index);
            if let Some(state) = self.files.get_mut(&path) {
                state.current = Some(FileVersion::from_observed(observed));
            }
            return Ok(());
        }
        if self.pending_events.len() >= self.options.max_pending_facts {
            return Err(ChangeTrackerError::BudgetExceeded {
                message: "pending filesystem event budget exhausted".into(),
            });
        }
        self.pending_events.push_back(PendingEvent {
            event: SemanticEvent { path, ..event },
            observed,
            expires: Instant::now() + self.options.causal_window,
        });
        Ok(())
    }

    fn flush_expired(&mut self) -> Result<(), ChangeTrackerError> {
        let now = Instant::now();
        self.pending_receipts
            .retain(|pending| pending.expires > now);
        let mut expired = Vec::new();
        while self
            .pending_events
            .front()
            .is_some_and(|pending| pending.expires <= now)
        {
            if let Some(pending) = self.pending_events.pop_front() {
                expired.push(pending);
            }
        }
        for pending in expired {
            self.apply_external(pending)?;
        }
        Ok(())
    }

    pub(super) fn flush_all_events(&mut self) -> Result<(), ChangeTrackerError> {
        while let Some(pending) = self.pending_events.pop_front() {
            self.apply_external(pending)?;
        }
        Ok(())
    }

    fn apply_external(&mut self, pending: PendingEvent) -> Result<(), ChangeTrackerError> {
        let path = pending.event.path;
        let previous_state = self.files.get(&path).cloned();
        let state = self.files.entry(path.clone()).or_default();
        let source = if state.agent_touched {
            ChangeSource::ExternalEditOnAgentFile
        } else {
            ChangeSource::ExternalEdit
        };
        let before_revision = state
            .current
            .as_ref()
            .and_then(|current| current.exists.then(|| current.revision.clone()));
        state.mutation_kind = match pending.event.kind {
            FsChangeKind::Created => "external_create",
            FsChangeKind::Modified => "external_edit",
            FsChangeKind::Removed => "external_delete",
            FsChangeKind::Renamed => "external_rename",
        }
        .into();
        state.current = Some(FileVersion::from_observed(pending.observed));
        if let Err(error) = self.recompute_and_record(path.clone(), source, None, before_revision) {
            match previous_state {
                Some(state) => {
                    self.files.insert(path, state);
                }
                None => {
                    self.files.remove(&path);
                }
            }
            return Err(error);
        }
        Ok(())
    }

    fn recompute_and_record(
        &mut self,
        path: PathBuf,
        latest_source: ChangeSource,
        latest_context: Option<TrackingContext>,
        fact_before_revision: Option<String>,
    ) -> Result<(), ChangeTrackerError> {
        self.ensure_fact_budget()?;
        let (baseline, current) = {
            let state = self
                .files
                .get(&path)
                .ok_or_else(|| ChangeTrackerError::InvalidFact {
                    message: format!("tracked state disappeared: {}", path.display()),
                })?;
            (state.baseline.clone(), state.current.clone())
        };
        let current = current.ok_or_else(|| ChangeTrackerError::InvalidFact {
            message: format!("tracked state has no current version: {}", path.display()),
        })?;
        if baseline
            .as_ref()
            .is_some_and(|baseline| baseline.same_identity(&current))
        {
            if let Some(state) = self.files.get_mut(&path) {
                state.snapshot = None;
                state.identities.clear();
            }
            return self.record_fact(
                path,
                latest_source,
                latest_context,
                fact_before_revision,
                current,
                Vec::new(),
            );
        }
        let unified_diff = baseline
            .as_ref()
            .and_then(|baseline| baseline.content.as_deref())
            .zip(current.content.as_deref())
            .and_then(|(before, after)| {
                bounded_unified_diff(
                    &path,
                    before,
                    after,
                    self.options.max_diff_bytes,
                    self.options.max_diff_lines,
                )
            });
        let parsed = parse_hunks(unified_diff.as_deref(), &current.revision);
        if parsed.len() > self.options.max_hunks_per_file {
            return Err(ChangeTrackerError::BudgetExceeded {
                message: format!(
                    "{} hunks exceed the per-file budget of {}",
                    parsed.len(),
                    self.options.max_hunks_per_file
                ),
            });
        }
        let history_bytes = parsed
            .iter()
            .filter_map(|hunk| hunk.diff.as_ref())
            .fold(0_usize, |total, diff| total.saturating_add(diff.len()));
        if self.history_bytes.saturating_add(history_bytes) > self.options.max_history_bytes {
            return Err(ChangeTrackerError::BudgetExceeded {
                message: format!(
                    "hunk history exceeds the {} byte budget",
                    self.options.max_history_bytes
                ),
            });
        }
        let state = self.files.entry(path.clone()).or_default();
        let old = std::mem::take(&mut state.identities);
        let mut used = vec![false; old.len()];
        let mut identities = Vec::with_capacity(parsed.len());
        let mut hunks = Vec::with_capacity(parsed.len());
        for parsed in parsed {
            let matched = best_identity_match(&parsed, &old, &used);
            let preserved = matched.filter(|index| old[*index].fingerprint == parsed.fingerprint);
            let id = if let Some(index) = matched {
                used[index] = true;
                old[index].id.clone()
            } else {
                let id = HunkId(format!("hunk-{:016x}", self.next_hunk));
                self.next_hunk = self.next_hunk.saturating_add(1);
                id
            };
            let (source, context, before_revision, after_revision) = preserved.map_or_else(
                || {
                    (
                        latest_source,
                        latest_context.clone(),
                        baseline.as_ref().and_then(|baseline| {
                            baseline.exists.then(|| baseline.revision.clone())
                        }),
                        current.revision.clone(),
                    )
                },
                |index| {
                    (
                        old[index].source,
                        old[index].context.clone(),
                        old[index].before_revision.clone(),
                        old[index].after_revision.clone(),
                    )
                },
            );
            identities.push(HunkIdentity {
                id: id.clone(),
                fingerprint: parsed.fingerprint,
                range: parsed.range,
                source,
                context: context.clone(),
                before_revision: before_revision.clone(),
                after_revision: after_revision.clone(),
            });
            hunks.push(HunkSnapshot {
                id,
                range: parsed.range,
                source,
                context,
                before_revision,
                after_revision,
                after_exists: current.exists,
                unified_diff: parsed.diff,
            });
        }
        state.identities = identities;
        let recorded_at = SystemTime::now();
        let recorded_sequence = self.next_fact;
        self.next_fact = self.next_fact.saturating_add(1);
        let before_revision = baseline
            .as_ref()
            .and_then(|baseline| baseline.exists.then(|| baseline.revision.clone()));
        let snapshot = TrackedFileSnapshot {
            recorded_sequence,
            path: path.clone(),
            target_fingerprint: state.target_fingerprint.clone(),
            before_revision: before_revision.clone(),
            after_revision: current.revision.clone(),
            after_exists: current.exists,
            source: latest_source,
            mutation_kind: state.mutation_kind.clone(),
            context: latest_context.clone(),
            hunks: hunks.clone(),
            updated_at: recorded_at,
        };
        state.snapshot = Some(snapshot);
        self.record_fact_with_sequence(
            recorded_sequence,
            recorded_at,
            path,
            latest_source,
            latest_context,
            fact_before_revision,
            current,
            hunks,
            history_bytes,
        )
    }

    fn record_fact(
        &mut self,
        path: PathBuf,
        source: ChangeSource,
        context: Option<TrackingContext>,
        before_revision: Option<String>,
        current: FileVersion,
        hunks: Vec<HunkSnapshot>,
    ) -> Result<(), ChangeTrackerError> {
        let recorded_sequence = self.next_fact;
        self.next_fact = self.next_fact.saturating_add(1);
        self.record_fact_with_sequence(
            recorded_sequence,
            SystemTime::now(),
            path,
            source,
            context,
            before_revision,
            current,
            hunks,
            0,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_fact_with_sequence(
        &mut self,
        recorded_sequence: u64,
        recorded_at: SystemTime,
        path: PathBuf,
        source: ChangeSource,
        context: Option<TrackingContext>,
        before_revision: Option<String>,
        current: FileVersion,
        hunks: Vec<HunkSnapshot>,
        history_bytes: usize,
    ) -> Result<(), ChangeTrackerError> {
        self.ensure_fact_budget()?;
        let target_fingerprint = self
            .files
            .get(&path)
            .and_then(|state| state.target_fingerprint.clone());
        let mutation_kind = self
            .files
            .get(&path)
            .map(|state| state.mutation_kind.clone())
            .unwrap_or_default();
        self.facts.push_back(ChangeFactSnapshot {
            recorded_sequence,
            path,
            target_fingerprint,
            before_revision,
            after_revision: current.revision,
            after_exists: current.exists,
            source,
            mutation_kind,
            context,
            hunks,
            recorded_at,
        });
        self.history_bytes = self.history_bytes.saturating_add(history_bytes);
        Ok(())
    }

    fn ensure_fact_budget(&self) -> Result<(), ChangeTrackerError> {
        if self.facts.len() >= self.options.max_change_facts {
            return Err(ChangeTrackerError::BudgetExceeded {
                message: format!(
                    "change fact budget of {} exhausted",
                    self.options.max_change_facts
                ),
            });
        }
        Ok(())
    }

    fn ensure_file_budget(&self, path: &Path) -> Result<(), ChangeTrackerError> {
        if !self.files.contains_key(path) && self.files.len() >= self.options.max_files {
            return Err(ChangeTrackerError::BudgetExceeded {
                message: format!(
                    "tracked file budget of {} exhausted",
                    self.options.max_files
                ),
            });
        }
        Ok(())
    }

    pub(super) fn snapshot(&mut self) -> Result<HunkTrackerSnapshot, ChangeTrackerError> {
        self.flush_expired()?;
        Ok(HunkTrackerSnapshot {
            files: self
                .files
                .values()
                .filter_map(|state| state.snapshot.clone())
                .collect(),
            facts: self.facts.iter().cloned().collect(),
            reconcile: self.reconcile,
            pending_receipts: self.pending_receipts.len(),
            pending_events: self.pending_events.len(),
        })
    }
}
