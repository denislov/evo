use super::*;

impl SessionTransactionWriter {
    pub(crate) fn new(
        store: SessionLogStore,
        handle: SessionHandle,
    ) -> Result<Self, CodingSessionError> {
        let key = writer_registry_key(&handle);
        let registry = SESSION_WRITER_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        let mut registry = registry.lock_resource("session writer registry")?;
        let closed_keys = registry
            .iter()
            .map(|(key, writer)| writer.is_open().map(|open| (!open).then(|| key.clone())))
            .collect::<Result<Vec<_>, _>>()?;
        for key in closed_keys.into_iter().flatten() {
            registry.remove(&key);
        }
        if let Some(inner) = registry.get(&key).cloned() {
            inner.acquire_owner();
            return Ok(Self::from_owner(inner));
        }

        let mut write_lease = store.acquire_write_lease(&handle)?;
        let initial_committed_sequence = write_lease.committed_sequence();
        let startup_storage_recoveries = write_lease.tail_recoveries().to_vec();
        let (sender, mut receiver) =
            mpsc::channel::<SessionTransactionWriterEnvelope>(SESSION_TRANSACTION_WRITER_CAPACITY);
        let snapshot = Arc::new(Mutex::new(handle.manifest().clone()));
        let worker_snapshot = snapshot.clone();
        let committed_session_sequence = Arc::new(AtomicU64::new(initial_committed_sequence));
        let worker_committed_session_sequence = committed_session_sequence.clone();
        #[cfg(test)]
        let command_delay_millis = Arc::new(AtomicU64::new(0));
        #[cfg(test)]
        let worker_command_delay_millis = command_delay_millis.clone();
        let worker = std::thread::spawn(move || {
            let mut handle = handle;
            while let Some(envelope) = receiver.blocking_recv() {
                #[cfg(test)]
                {
                    let delay = worker_command_delay_millis.load(Ordering::Acquire);
                    if delay != 0 {
                        std::thread::sleep(Duration::from_millis(delay));
                    }
                }
                let result =
                    execute_writer_command(&store, &mut handle, &mut write_lease, envelope.command);
                let result = result.and_then(|receipt| {
                    *worker_snapshot.lock_resource("session writer manifest snapshot")? =
                        handle.manifest().clone();
                    Ok(receipt)
                });
                if let Ok(receipt) = result.as_ref()
                    && let Some(sequence) = receipt.committed_session_sequence
                {
                    worker_committed_session_sequence.fetch_max(sequence, Ordering::AcqRel);
                }
                let _ = envelope.reply.send(result);
            }
        });
        let inner = Arc::new(SessionTransactionWriterInner {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            owners: AtomicUsize::new(1),
            #[cfg(test)]
            last_owner_release_pause: Mutex::new(None),
            #[cfg(test)]
            command_delay_millis,
            #[cfg(test)]
            enqueue_timeout_millis: AtomicU64::new(
                SESSION_TRANSACTION_ENQUEUE_TIMEOUT.as_millis() as u64
            ),
            snapshot,
            committed_session_sequence,
            startup_storage_recoveries,
            registry_key: key.clone(),
        });
        registry.insert(key, inner.clone());
        Ok(Self::from_owner(inner))
    }

    fn from_owner(inner: Arc<SessionTransactionWriterInner>) -> Self {
        Self {
            owner: Arc::new(SessionWriterOwnerLease {
                inner: Arc::downgrade(&inner),
                released: AtomicBool::new(false),
            }),
            inner,
        }
    }

    fn sender(&self) -> Result<mpsc::Sender<SessionTransactionWriterEnvelope>, CodingSessionError> {
        self.inner
            .sender
            .lock_resource("session transaction writer sender")?
            .as_ref()
            .cloned()
            .ok_or_else(|| CodingSessionError::Session {
                message: "session transaction writer is closed".into(),
            })
    }

    fn enqueue_async(
        &self,
        command: SessionTransactionWriterCommand,
    ) -> BoxFuture<
        '_,
        Result<
            oneshot::Receiver<Result<SessionCommitReceipt, CodingSessionError>>,
            CodingSessionError,
        >,
    > {
        Box::pin(async move {
            let (reply, response) = oneshot::channel();
            let envelope = SessionTransactionWriterEnvelope { command, reply };
            let sender = self.sender()?;
            let enqueue_timeout = self.enqueue_timeout();
            match tokio::time::timeout(enqueue_timeout, sender.send(envelope)).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    return Err(CodingSessionError::Session {
                        message: "session transaction writer is closed".into(),
                    });
                }
                Err(_) => return Err(queue_saturated_error(enqueue_timeout)),
            }
            Ok(response)
        })
    }

    fn enqueue_blocking(
        &self,
        command: SessionTransactionWriterCommand,
    ) -> Result<
        oneshot::Receiver<Result<SessionCommitReceipt, CodingSessionError>>,
        CodingSessionError,
    > {
        let (reply, response) = oneshot::channel();
        let mut envelope = SessionTransactionWriterEnvelope { command, reply };
        let sender = self.sender()?;
        let enqueue_timeout = self.enqueue_timeout();
        let deadline = Instant::now() + enqueue_timeout;
        loop {
            match sender.try_send(envelope) {
                Ok(()) => return Ok(response),
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return Err(CodingSessionError::Session {
                        message: "session transaction writer is closed".into(),
                    });
                }
                Err(mpsc::error::TrySendError::Full(returned)) => {
                    if Instant::now() >= deadline {
                        return Err(queue_saturated_error(enqueue_timeout));
                    }
                    envelope = returned;
                    std::thread::sleep(SESSION_TRANSACTION_BLOCKING_RETRY_INTERVAL);
                }
            }
        }
    }

    fn enqueue_timeout(&self) -> Duration {
        #[cfg(test)]
        {
            Duration::from_millis(self.inner.enqueue_timeout_millis.load(Ordering::Acquire))
        }
        #[cfg(not(test))]
        {
            SESSION_TRANSACTION_ENQUEUE_TIMEOUT
        }
    }

    #[cfg(test)]
    pub(super) fn set_command_delay(&self, delay: Duration) {
        self.inner
            .command_delay_millis
            .store(delay.as_millis() as u64, Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn set_enqueue_timeout(&self, timeout: Duration) {
        self.inner
            .enqueue_timeout_millis
            .store(timeout.as_millis() as u64, Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn remaining_queue_capacity(&self) -> usize {
        self.sender().map_or(0, |sender| sender.capacity())
    }

    pub(super) async fn execute_async(
        &self,
        command: SessionTransactionWriterCommand,
    ) -> Result<SessionCommitReceipt, CodingSessionError> {
        self.enqueue_async(command)
            .await?
            .await
            .map_err(|_| CodingSessionError::Session {
                message: "session transaction writer closed before replying".into(),
            })?
    }

    fn execute_blocking(
        &self,
        command: SessionTransactionWriterCommand,
    ) -> Result<SessionCommitReceipt, CodingSessionError> {
        self.enqueue_blocking(command)?
            .blocking_recv()
            .map_err(|_| CodingSessionError::Session {
                message: "session transaction writer closed before replying".into(),
            })?
    }

    pub(crate) async fn append_checkpoint_events(
        &self,
        events: Vec<SessionEventEnvelope>,
    ) -> Result<(), CodingSessionError> {
        self.append_checkpoint_events_with_receipt(events)
            .await
            .map(|_| ())
    }

    pub(crate) async fn append_checkpoint_events_with_receipt(
        &self,
        events: Vec<SessionEventEnvelope>,
    ) -> Result<SessionCommitReceipt, CodingSessionError> {
        self.execute_async(SessionTransactionWriterCommand::Checkpoint { events })
            .await
    }

    #[cfg(test)]
    pub(crate) fn append_checkpoint_events_blocking(
        &self,
        events: Vec<SessionEventEnvelope>,
    ) -> Result<(), CodingSessionError> {
        self.execute_blocking(SessionTransactionWriterCommand::Checkpoint { events })
            .map(|_| ())
    }

    pub(crate) fn append_checkpoint_events_with_receipt_blocking(
        &self,
        events: Vec<SessionEventEnvelope>,
    ) -> Result<SessionCommitReceipt, CodingSessionError> {
        self.execute_blocking(SessionTransactionWriterCommand::Checkpoint { events })
    }

    pub(crate) fn initialize_session_with_receipt_blocking(
        &self,
        event: SessionEventEnvelope,
    ) -> Result<SessionCommitReceipt, CodingSessionError> {
        self.execute_blocking(SessionTransactionWriterCommand::InitializeSession { event })
    }

    pub(crate) async fn commit_session_mutation(
        &self,
        events: Vec<SessionEventEnvelope>,
        manifest_patch: ManifestPatch,
        operation_id: Option<String>,
    ) -> Result<SessionCommitReceipt, CodingSessionError> {
        self.commit_session_mutation_with_outbox(events, Vec::new(), manifest_patch, operation_id)
            .await
    }

    pub(crate) async fn commit_session_mutation_with_outbox(
        &self,
        events: Vec<SessionEventEnvelope>,
        outbox_records: Vec<DurableOutboxRecordCandidate>,
        manifest_patch: ManifestPatch,
        operation_id: Option<String>,
    ) -> Result<SessionCommitReceipt, CodingSessionError> {
        self.execute_async(SessionTransactionWriterCommand::CommitSessionMutation {
            events,
            outbox_records,
            manifest_patch,
            operation_id,
        })
        .await
    }

    pub(crate) async fn commit_session_name_if_unset(
        &self,
        events: Vec<SessionEventEnvelope>,
        manifest_patch: ManifestPatch,
        operation_id: String,
    ) -> Result<SessionCommitReceipt, CodingSessionError> {
        self.execute_async(SessionTransactionWriterCommand::CommitSessionNameIfUnset {
            events,
            manifest_patch,
            operation_id,
        })
        .await
    }

    pub(crate) fn commit_session_mutation_blocking(
        &self,
        events: Vec<SessionEventEnvelope>,
        manifest_patch: ManifestPatch,
        operation_id: Option<String>,
    ) -> Result<SessionCommitReceipt, CodingSessionError> {
        self.execute_blocking(SessionTransactionWriterCommand::CommitSessionMutation {
            events,
            outbox_records: Vec::new(),
            manifest_patch,
            operation_id,
        })
    }

    pub(crate) fn commit_session_mutation_with_outbox_blocking(
        &self,
        events: Vec<SessionEventEnvelope>,
        outbox_records: Vec<DurableOutboxRecordCandidate>,
        manifest_patch: ManifestPatch,
        operation_id: Option<String>,
    ) -> Result<SessionCommitReceipt, CodingSessionError> {
        self.execute_blocking(SessionTransactionWriterCommand::CommitSessionMutation {
            events,
            outbox_records,
            manifest_patch,
            operation_id,
        })
    }

    pub(crate) fn manifest_snapshot(&self) -> Result<SessionManifest, CodingSessionError> {
        Ok(self
            .inner
            .snapshot
            .lock_resource("session writer manifest snapshot")?
            .clone())
    }

    pub(crate) fn committed_session_sequence(&self) -> u64 {
        self.inner
            .committed_session_sequence
            .load(Ordering::Acquire)
    }

    pub(crate) fn startup_storage_recoveries(&self) -> &[String] {
        &self.inner.startup_storage_recoveries
    }

    pub(crate) fn shutdown(&self) -> Result<(), CodingSessionError> {
        self.owner.release()
    }
}
