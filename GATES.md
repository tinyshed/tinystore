# Gates

Each promise the repository keeps, by engine, and what fails when it is
broken: a test, a lint or a step of the build. A change that makes a promise
adds its line here, with the test that keeps it, and
`TestEveryGateNamesATestThatExists` fails when a line names a Go test that no
module defines.

## The repository

- nothing switches off the linker's method pruning — the canary step of `task size`, over `internal/linkaudit`
- no cgo — `CGO_ENABLED=0` in the build, on all three CI platforms
- the module carries only the engine — `TestTheModuleCarriesOnlyTheEngine`, over its own go.mod
- importing this stays cheap — `task size` links a cgo-free linux/amd64 probe and reports what it cost
- a program may link another SQLite driver beside it — `TestTheExampleRegistersNoSQLDriver` in `examples/notes`: no name registered
- a `//nolint` silences a named finding and says why — `nolintlint`: no unused, unexplained or blanket directive
- a gate here names a test that exists — `TestEveryGateNamesATestThatExists`, over every module's tests
- a skill's pointer says what its skill says — `TestEverySkillHasAPointerThatMatchesIt`
- the workflows parse before a release needs them — `task lint:actions`, in CI's quality job
- the root links no engine — `TestTheRootImportsNoEngine`
- engines never import each other, nor the server — `TestEnginesDoNotImportEachOther`, over every engine package

## The store

- a container's memory limit gives the store's budget, and no limit gives none — `TestFromCgroupIsAFractionOfTheContainersLimit`
- every limit a LimitError names is an exported constant, the SDKs' limits the same names — `TestEveryLimitNameIsAnExportedConstant`, over `testdata/limits.json`; `are the names of testdata/limits.json, which Go's constants hold` in `sdk/js/test/wire.test.ts`, `test_limits_are_the_names_of_the_shared_file` in Python's
- one store holds a directory — `TestASecondStoreOnTheSameDirectoryIsRefused`
- engines close last opened first, once — `TestCloseClosesEnginesLastOpenedFirstAndOnlyOnce`
- a repeated background failure is not a log flood — `TestBackgroundFailuresAreLoggedOncePerQuietPeriod`; `a repeated failure is said once a quiet period, and its recovery once` in `sdk/js/test/background.test.ts`, `test_a_repeated_failure_is_said_once_a_quiet_period_and_its_recovery_once` in Python's
- self-metrics are opt-in, the last report before the engines close — `TestSelfMetricsAreOptInAndCollectBeforeClose`
- a closed gate drains before an engine closes — `TestAClosedGateRefusesWorkAndDrainsWhenTheWorkLeaves`
- a large reservation is not passed over by small ones — `TestMemoryGrantsInArrivalOrder` and `TestMemoryCancelledWaiterLetsTheNextOneIn`
- a reservation shrinks to what its work holds — `TestAReservationShrinksToWhatItHolds`
- work holding a writer never waits for memory — `TestAReservationThatCannotWaitTakesOnlyWhatIsFree`
- a limit names which, what the call wanted and the bound — `TestALimitErrorNamesItsBoundAndIsItsKind`, `TestWholeExactBlocksNeedNoDecodedSampleBudget`

## Codec

- the extremes survive too — `TestExactBitsAndTimestampExtremes`, on `-0`, NaN payloads and the ends of time
- a corrupt payload is refused — `TestPayloadCorruptionIsRefused` and `FuzzDecode`
- bytes written once still read — `TestPayloadsWrittenBeforeStillRead`, one vector per representation
- a head that does not fit its body is refused — the four moved heads in `TestPayloadCorruptionIsRefused`
- a decimal travels as the integer it was written as — `TestDecimalsTravelAsTheIntegersTheyWereWrittenAs`
- a value no scale reproduces is refused, not rounded — `TestAValueNoScaleReproducesIsRefusedRatherThanRounded`
- a decode stays bounded — `TestADecodeStaysWithinTheBodysCeiling`, against the 8 KiB ceiling
- an iterator outlives its codec — `TestIteratorOwnsItsBytesAndOutlivesTheCodec`
- unordered or oversized input is refused — `TestRejectsUnorderedAndOversizedInput`

## Metrics

- an engine's errors are the store's kinds — `TestMetricsErrorsAreTheStoresKinds`
- a failed engine open gives its file back — `TestMetricsOpensOncePerStoreAndAFailedOpenLetsGo`
- one refused instrument does not keep the others out — `TestARefusedInstrumentDoesNotKeepTheOthersOut`; `a refused instrument keeps no other out, and says so once` in `sdk/js/test/records.test.ts`, `test_a_refused_instrument_keeps_no_other_out_and_says_so_once` in Python's
- an instrument's last value survives Close — `TestClosingTheStoreFlushesTheLastValues`
- a timer writes its count, sum and longest at each flush — `TestATimerWritesItsCountSumAndLongestAtEachFlush`, `TestAFailedFlushKeepsATimersLongestForTheNext`
- a timer's three series go together and have one writer — `TestATimersSeriesHaveOneWriter`, `TestARefusedTimerLeavesItsSeriesOutTogether`, `TestATimerTheSeriesLimitCutsWritesNoneOfItsSeries`
- a self-report never counts itself, nor rounds a value — `TestSelfSamplesDoNotCountThemselvesAndSurviveClose`, `TestSelfMetricsRefuseAmbiguousOrInexactReports`
- a sample survives the codec exactly — `TestEveryValueRepresentationPreservesBits`, on bits and not on values
- a sample survives a file and restart — `TestHeadSealingReopenAndPartialRetention`, over the public metrics API
- publication is one write — `TestFailedPublicationRollsBackPayloadsHeadAndIdentifiers`
- a reader sees one consistent state — `TestReadersSeeOneSnapshotWhilePackingAndIngesting`, including retention
- shared clocks live as long as their owners — `TestClockSharingAndLastOwnerRetention`
- production stays independent of research — `TestEngineDoesNotImportExperimentsOrFutureEngines`
- a counter's increase survives a reset — `TestCounterSummaryIncludesResets`, `TestAggregateCounterIncludesBlockTransitionButNotBucketTransition`
- only the safe prefix is sealed — `TestWatermarkIsStrictAndFollowsTheSeries`, on the strict edge
- a late sample cannot enter a sealed block — `TestHeadSealingReopenAndPartialRetention`, `ErrTooOld` behind the frontier
- a partial range is not answered from a summary — `TestWholeSummarySkipsPayloadButPartialBlocksCheckIt`: a cut block decodes raw
- retention clips before it summarises — `TestAggregateClipsRetentionBeforeSummingSealedEdges`
- a bucket retention cut says so — `TestAggregateMarksOnlyTheBucketRetentionCut`
- a quiet tail expires without becoming a block — `TestHeadSealingReopenAndPartialRetention`, the one-sample head at its end
- one expired sample does not delete a block — `TestHeadSealingReopenAndPartialRetention`, the partly expired second block
- one damaged series does not stop its neighbors — `TestCorruptSeriesDoesNotStopOtherMaintenance`
- a raised limit can resume suspended maintenance — `TestSuspendedLimitCanRecoverAfterReopen`
- an append need not churn due indexes — `TestExistingSeriesIngestAvoidsUnchangedDueIndexes`
- untouched head chunks keep their bytes — `TestUnchangedHeadChunksKeepTheirEncodedBytes`
- a posting above the old cap still ranks exactly — `TestPostingCountsRankAboveTheOldProbeCap`
- ready waits for a safe prefix — `TestReadyWaitsForASealableWatermarkPrefix`
- one bad publication does not count its neighbors — `TestPublicationBatchRollsBackOneConflictingSeries` and `TestPublicationBatchDoesNotCountRolledBackTransaction`
- cancelled callers do not bypass active-work slots — `TestActiveReadAndIngestAdmissionHonorsCancellation`, `TestSlotsHonourCancellation`
- expired series return a cardinality slot — `TestExpiredSeriesReclaimsCardinalityAndAllowsNewLifecycle`
- live siblings keep shared dictionary pairs — `TestReclaimKeepsLabelsUsedByAnotherSeries`
- narrow reads pay only for selected packed chunks — `TestNarrowPackedHeadChargesSelectedChunksAndChecksWholeChecksum` and `TestBatchedNarrowHeadsChargeSelectedChunks`
- an engine's work honors the store's memory — `TestStoreMemoryBoundsReadsIngestAndMaintenance`
- streaming owns each result and exposes partial failure — `TestStreamOwnsResultsAndReportsPartialFailure`
- an error about one series carries its labels — `TestIngestRefusalNamesItsSeries` and `TestIngestValidationNamesItsSeries`
- a metrics plan is what the call then spends — `TestAPlanSaysWhatTheCallThenSpends`, no payload fetched
- a series is its name and its labels, `__` the store's — `TestASeriesIsItsNameAndItsLabels`, `TestIngestIsAtomicAndLastMutableValueWins`
- a range over the last Since starts that long before now — `TestARangeSinceStartsThatLongBeforeNow`, a To of zero the open end
- a condition finds series beyond equality, NoneOf alone refused — `TestConditionsFindSeriesBeyondEquality`, `TestAConditionThatFindsNothingOrCannotBeTakenIsRefused`, `TestConditionsOverTheWire`
- a series that cannot be repaired can still be dropped — `TestDropSeriesRemovesAnUnreadableSuspendedSeries` and `TestDropSeriesKeepsItsNeighbours`
- long-head append preserves bits and frontier — `TestLongPackedHeadAppendKeepsExactBitsAndFrontier`
- exact aggregates cross blocks, resets and retention — `TestAggregateRoundsExactSumAcrossSealedBlocks`, `TestAggregateCounterIncludesBlockTransitionButNotBucketTransition` and `TestAggregateClipsRetentionBeforeSummingSealedEdges`
- a whole block's summary answers as its samples do — `TestSummaryAndRawAggregatesAgreeAtEveryBoundary`, every operation, kind and boundary
- a group joins its series exactly and rounds once — `TestAGroupJoinsItsSeriesExactlyAndRoundsOnce`, `TestRateAndDeltaAreExactPerSeriesThenJoined`
- a whole-block summary spends no decoded-sample budget — `TestWholeExactBlocksNeedNoDecodedSampleBudget`
- a malformed exact summary is refused — `TestExactSummaryEncodingRefusesNoncanonicalOrUnboundedFields`, `FuzzExactSummary`
- exact summaries keep a directory within its bound — `TestLargeExactSummariesStayWithinDirectoryBounds`
- a directory written once still reads — `TestDirectoriesWrittenBeforeStillRead`, inline and external

## Records

- a log line never waits for the file — `TestAFullBufferDropsAndCountsWithoutWaiting`
- what a full buffer dropped is said, once a quiet period — `TestDroppedLinesAreSaidOncePerQuietPeriod`; `dropped lines are said at a flush, once a quiet period, counted since the last time` in `sdk/js/test/background.test.ts`, `test_dropped_lines_are_said_at_a_flush_once_a_quiet_period_counted_since_the_last_time` in Python's
- a half-full log buffer is written before its interval — `TestAHalfFullBufferFlushesBeforeItsInterval`; `test_a_half_full_buffer_is_written_before_its_interval` in Python's
- writing a log does not log again — `TestTheEnginesOwnLinesAreRefused`
- a logger's console line is the same bytes in Go, Bun and Python, its time and stream as it is told — `TestConsoleLinesAreTheVectors`, over `records/console/testdata/console.json`, which both SDKs' suites read
- LOG_LEVEL, LOG_FORMAT and LOG_TIME win over a logger's code, and never turn a console on — `TestTheEnvironmentWinsOverTheOptions`; `win over the options, and never turn on a console turned off` in `sdk/js/test/logger.test.ts`, `test_the_environment_wins_over_the_arguments_and_never_turns_a_console_on` in Python's
- a LOG_ value no one can mean is ignored, and said once — `TestAValueTheEnvironmentCannotMeanIsIgnoredAndSaidOnce`; `a value they cannot mean is ignored, and said once` in `sdk/js/test/logger.test.ts`, `test_a_value_the_environment_cannot_mean_is_ignored_and_said_once` in Python's
- a logger writes its console where it is told — `TestToWritesTheConsoleWhereItIsTold`; `a logger writes where to says: JSON off a terminal, pretty on one` in `sdk/js/test/logger.test.ts`, `test_a_handler_writes_where_to_says_json_off_a_terminal` in Python's
- a line reaches its console as it is logged, whole — `TestEachLineReachesTheConsoleAsItIsLogged`, `TestConsoleLinesFromManyGoroutinesDoNotInterleave`
- the engine's own lines reach the console from Info up, never the store — `TestTheEnginesOwnLinesReachTheConsoleAndNotTheStore`
- a redacted field is kept nowhere — `TestARedactedFieldIsHiddenInTheStoreAndOnTheConsole`, and in both SDKs' suites
- a secret is hidden whatever its key's spelling, and a counter that looks like one is not — `TestRedactHidesASecretWhateverItsKeySpelling`, `TestACounterNamedLikeASecretStaysVisible`, and the vectors of `records/console/testdata/console.json`, whose `secrets` each SDK's list equals
- a URL's password inside a value is hidden unless kept — `TestAPasswordInsideAURLIsHidden`, `TestAURLsPasswordIsHiddenUnlessKept`; `test_a_urls_password_is_hidden_unless_kept` in Python's
- a host that named its prefix reads only its own variables, a lookup only the host's function, NoEnv none — `TestAHostThatNamedItsPrefixIgnoresTheBareVariables`, `TestALookupIsTheOnlyEnvironmentRead`, `TestNoEnvReadsNothing`; `a logger that named its prefix reads its own variables and not the bare ones` in `sdk/js/test/console.test.ts`, `test_a_handler_that_named_its_prefix_ignores_the_bare_variables` in Python's
- ReplaceAttr sees each attribute with its groups, and drops a zero one — `TestReplaceAttrChangesAndDropsAttributes`
- a source is where the line was logged — `TestASourceIsWhereTheLineWasLogged`; `says where a line was logged when asked` in `sdk/js/test/console.test.ts`, `test_source_says_where_a_line_was_logged` in Python's
- a logger keeps its lines from its level up — `TestALevelKeepsALoggersLinesFromItUp`
- a logger without a store writes the console alone, and links no store — `TestAHandlerWritesTheConsoleAlone`, `TestTheConsoleImportsNoStore`
- colours need a terminal, and not NO_COLOR — `TestAFileIsNoTerminal`, `TestNoColorAndADumbTerminalTurnColoursOff`
- FORCE_COLOR colours a pipe, and NO_COLOR still wins — `TestForceColorTurnsColoursOnWithoutATerminal`; `makes a pipe pretty and coloured, unless NO_COLOR` in `sdk/js/test/logger.test.ts`, `test_force_color_makes_a_pipe_pretty_and_coloured_unless_no_color` in Python's
- a line one record holds loses no byte; a longer one is dropped and counted — `FuzzLinesLoseNoByte`, `TestLinesKeepEveryByte`, `TestALargeLineWriteKeepsOnlyOneBoundedPartial`
- a writer of lines never waits, and closes with the store — `TestLinesNeverWaitAndCloseWithTheStore`
- a stack trace's lines make one record — `TestLinesJoinWhatBelongsTogether`, `TestAStackTraceGoesOn`
- a line's level is found where its program writes it — `TestLinesFindTheLevelWhereProgramsWriteIt`
- a level in colour counts as one in brackets — `TestAColourMarksALevelAsBracketsDo`
- a logfmt line keeps its pairs when they spell it — `TestLogfmtLinesKeepTheirPairsWhenTheySpellTheLineAgain`, `TestLinesKeepALogfmtLinesPairs`
- a record survives the records format exactly — `TestSegmentsWrittenBeforeStillRead`, `TestHeadRowsWrittenBeforeStillRead`
- a changed records byte is refused — `TestAChangedOrMissingByteIsRefused`, `TestAChangedHeadRowIsRefused`, fuzzers
- a records decode stays bounded — `TestExpandedTextIsBounded`; every copy is charged before it is made
- a time a line spells comes back as it was spelled — `TestEveryStampLayoutSpellsItsTextBack`, `FuzzStamps`
- what no calendar shows stays text — `TestATimeNoClockShowsStaysText`
- a time kept against its record needs that record — `TestStampsWithoutTheirTimesAreRefused`
- equal times keep their arrival order — `TestEqualTimesKeepTheirArrivalOrder`
- a read merges segments and the head by time — `TestReadMergesSegmentsAndTheHeadInEventTimeOrder`
- an appended record reads before it is sealed — `TestAppendedRecordsAreReadBeforeTheyAreSealed`
- a reader finds each record once while sealing — `TestReadersSeeEveryRecordOnceWhileSealing`
- a failed seal leaves the head as it was — `TestAFailedSealLeavesTheHeadAsItWas`
- an abrupt exit loses no appended record — `TestAnAbruptExitKeepsEveryAppendedRecord`, before, inside and after a seal
- a damaged head does not stop the others — `TestADamagedHeadDoesNotStopTheOthers`
- a damaged row is logged once, its head still seals — `TestADamagedHeadRowIsReportedOnceAndTheRestOfItsHeadSeals`
- a damaged segment is dropped whole, followed past — `TestDropRemovesADamagedSegmentAndFollowPassesIt`
- a follower keeps its place across merges — `TestAFollowerKeepsItsPlaceAcrossMerges`, the middle of a merged place included
- a merge is one write, and readers see a record once — `TestAFailedMergeLeavesTheSegmentsAsTheyWere`, `TestReadersSeeEveryRecordOnceWhileMerging`
- a merged segment goes whole, with its places — `TestAMergedSegmentExpiresWithItsPlaces`, `TestDropRemovesAMergedSegmentWithItsPlaces`
- a holder's places are found through an index — `TestPlacesAreFoundThroughTheirHolder`, on the plan SQLite chooses
- a follower far behind fetches a merged block once — `TestAFollowerFetchesAMergedBlockOnce`, `TestTheFollowCacheKeepsWhatIsUsedWithinItsBytes`
- a block's id is never given twice — `TestABlockIDIsNeverGivenTwice`
- a merge joins four of a size, in time order — `TestSmallSegmentsMergeFourOfASize`, `TestAMergeLeavesOverlappingSegmentsOut`
- only what no longer reads can be dropped — `TestDropRefusesWhatStillReads`
- a late record seals from its own head — `TestLateRecordsSealFromTheirOwnHead`
- a record appended alone can be late — `TestARecordAppendedAloneCanBeLate`
- a batch in time order makes none of its own late — `TestARecordTenSecondsBehindItsStreamsNewestIsLate`
- a reopened store knows what its heads hold — `TestAReopenedStoreKnowsWhatItsHeadsHold`
- a producer ahead of the store makes no one late — `TestAProducerAheadOfTheStoreDoesNotMakeItsNeighboursLate`
- a time outside its engine's window is refused — `TestARecordOutsideItsWindowIsRefused`, `TestASampleAheadOfTheClockIsRefused`
- a page never splits a timestamp nor loses one — `TestAPageNeverSplitsATimestamp`, `TestPagesContinueWithoutLosingOrRepeating`
- a budget ends a page rather than failing it — `TestABudgetEndsAPageEarly`
- a records scan since a span pages on in that range — `TestAScanSinceStartsThatLongBeforeNowAndPagesOn`, the clock moved between pages
- `All` walks every record a page at a time — `TestAllWalksEveryRecordAPageAtATime`, a walk stopped early reads no further
- a search finds a record by its text, the case ignored — `TestASearchFindsARecordByItsTextItsCaseIgnored`, sealed and in its head
- a record takes the trace of its context — `TestARecordTakesTheTraceOfItsContext`, the caller's records left unchanged
- blooms and level masks skip blocks — `TestBloomsAndLevelMasksSkipBlocks`
- a read walks the time index near its range only — `TestTheTimeIndexIsWalkedWithinEachSpan`, on the plan SQLite chooses
- a read finds records in blocks of every width — `TestReadsFindRecordsInBlocksOfEveryWidth`
- retention removes whole segments, clips reads — `TestRetentionRemovesWholeSegmentsAndClipsReads`
- records work holds the store's memory — `TestStoreMemoryBoundsAppendReadSealAndFollow`
- a follower is told what retention removed — `TestFollowCountsWhatRetentionRemovedFirst`
- records reads and appends wait for their slots — `TestReadsAndAppendsWaitForTheirSlots`
- one Append carries at most a segment's input — `TestAnAppendOfMoreThanASegmentIsRefused`
- the tool prints a record as a logger's console does — `TestAPrinterWritesARecordAsTheConsoleDoes` in `records`

## SQL

- an application's read cannot write — `TestAReadCannotWriteAndSaysWhereToWrite`
- an application's writes share a commit, fail alone — `TestExecsShareACommitAndFailAlone`, `TestAPanicInsideAWriteRollsBackItsStatementAlone`
- a batch commits whole in its group, and fails alone — `TestABatchCommitsTogetherAndFailsAlone`, `TestABatchThatDoesNotBuildWritesNothing`
- an applied migration cannot change under the file — `TestMigrationsApplyOnceAndAChangedOneRefuses`
- a migration rebuilding a parent keeps its children — `TestARebuiltTableKeepsItsChildren`, `TestMigrationsRunWithoutForeignKeysAndCheckThemBeforeCommit`
- a schema is the SQL it prints — `TestASchemaIsTheSQLItPrints`, golden; `TestANameSQLWouldMisreadIsQuoted`
- a declaration that cannot be a table fails at start — `TestADeclarationThatCannotBeATableFailsAtStart`
- an sqldb value comes back as it went in — `TestEveryValueComesBackAsItWentIn`, `TestArgumentsAreWrittenByTheirGoType`
- sixteen bytes of a type sqldb does not know are bytes — `TestOnlyAKnownUUIDTypeIsText`
- the standard library's uuid is text, as a field and as an argument — `TestAStandardLibraryUUIDIsKeptAsText`
- FTS5 and R*Tree work in an application's file and its snapshot — `TestFullTextAndRTreeTablesWorkInTheFileAndItsSnapshot`
- a virtual table of a module the program did not link is refused, naming its import — `TestAVirtualTableOfAModuleNotLinkedIsRefused`, `TestModuleOfReadsTheModule`
- a virtual table's shadow tables are not the application's — `TestAVirtualTablesShadowsAreNotTheSchemas`
- a value that does not decode names column and field — `TestAValueThatDoesNotDecodeNamesItsColumnAndField`
- a numbered SQL parameter without its argument is refused — `TestAStatementWithNumberedParametersNeedsEveryArgument`
- a value SQLite would change is refused — `TestAValueSQLiteWouldChangeIsRefused`: NaN, `uint64` past `int64`, a day no calendar has
- `Insert` writes every field but the generated ones — `TestInsertWritesEveryFieldButTheGeneratedOnes`, `TestInsertReturnsWhatTheDatabaseGenerated`
- `Open` checks the file and changes nothing — `TestOpenChecksTheFileAgainstTheSchemaAndChangesNothing`, `TestOpenNamesEachDifferenceOfStructure`
- an expression spelled otherwise never refuses a file — `TestOpenDoesNotRefuseAnExpressionSpelledOtherwise`
- a partial unique index is declared and checked, its condition's spelling a line — `TestAPartialUniqueIndexIsDeclaredAndChecked`, `TestOpenRefusesAPartialIndexWithTheDeclaredName`
- a long transaction fails no grouped write behind it — `TestALongTransactionFailsNoWriteBehindIt`, `TestALongTransactionFailsNoGroupedWriteBehindIt`
- a call on the DB inside its own Tx ends with its context — `TestACallOnTheDBInsideItsOwnTxEndsWithItsContext`
- an sqldb snapshot ends at its bound and says so — `TestEachHoldsOneSnapshotAndOneRow`, `TestASnapshotHeldPastItsBoundSaysSo`
- a constraint says its kind — `TestAConstraintSaysItsKind`
- a statement is compiled once a connection — `TestAStatementIsCompiledOnceAConnection`, `TestAConnectionKeepsTheStatementsItsFileWasOpenedWith`
- sqldb holds the store's memory before it decodes — `TestStoreMemoryBoundsReadsAndWrites`, `TestAllPastItsBoundRefuses`
- a schema check writes a migration only when asked — `TestCheckSchemaFindsWhatIsMissingAndWritesOnlyWhenAsked`, `TestTwoChecksOfOneNameFail`
- an ambiguous change is a draft that does not run — `TestAnAmbiguousChangeIsADraftThatDoesNotRun`
- a database no one opened is copied without opening it — `TestCopyTakesADatabaseNoOneOpened` in `sqldb`
- a value SQLite would make past the store's memory is refused — `TestAValueSQLiteWouldMakePastTheStoresMemoryIsRefused`
- `ApplyNone` and `Migrated` apply nothing — `TestApplyNoneAndMigratedApplyNothing`, `TestVerifyChecksTheHistoryAndRunsNothing`
- a statement SQLite refuses is `ErrInvalid` — `TestAStatementSQLiteRefusesIsInvalid`, a missing argument included
- sqldb reads rows without a struct — `TestQueryReadsRowsAsSQLiteReturnsThem`
- a database opens without migrations as it is, checking nothing — `TestADatabaseOpensWithoutMigrations`, `TestClaimStampsAFreshFileAndRunsNothing`; `a database opens without migrations, empty, to try a query` in `sdk/js/test/sql.test.ts`, `test_a_database_opens_without_migrations_empty_to_try_a_query` in Python's

## SQLite under every engine

- a reader beyond one closes once idle, and the next read opens one that still refuses to write — `TestAnIdleReaderClosesAndTheNextReadOpensIt`
- the store's Options.Readers bounds what an engine opens — `TestTheStoresReadersBoundWhatAnEngineWants`, `TestTheStoresReadersCapEveryDatabase`
- a guest engine keeps its history in its owner's file — `TestAGuestKeepsItsOwnHistoryInItsOwnersFile`
- a value comes back as SQLite keeps it, no time read into its text — `TestAValueComesBackAsSQLiteKeepsIt`, `TestAValueTravelsAsItsRowKeepsIt` in `server`
- bytes a caller scanned are its own — `TestABlobScannedAgainLeavesTheLastOnesBytes`
- a call prepares one statement and refuses a second — `TestACallPreparesOneStatement`, `TestSQLiteEndsAStatementWhereTheCheckDoes`
- a file's pages are divided with none counted twice or left over — `TestEveryPageIsCountedOnce`, `TestAPageCountedTwiceOrNeverIsRefused`
- a group's leader whose caller left hands the lead on — `TestALeaderWhoseCallerLeavesHandsTheLeadOn`
- a snapshot does not stop the writer — `TestSnapshotCopiesWhileTheWriterWrites`
- writer programs stay bounded and transactional — `TestPreparedWriterUsesOneTransactionAndRetainsPrograms` and `TestPreparedWriterCacheStaysBounded`
- writes queued for the writer share a commit, fail alone — `TestGroupedWritesShareACommitAndFailAlone`
- a commit gathers the writers its last one answered — `TestAGroupGathersTheWritesItsLastBatchAnswered`
- a write whose caller left before its turn writes nothing — `TestACallerCancelledBeforeItsTurnWritesNothing`
- a grouped write that has started finishes with its group — `TestAWriteThatHasStartedFinishesWithItsGroup`, cancelled or past its deadline mid-statement
- the application's grouped SQL ends at its deadline — `TestAStatementUntilItsDeadlineEndsThere`, `TestADataStatementEndsAtItsDeadline`
- a statement by key starts no goroutine, nor runs once its context ended — `TestAStatementByKeyStartsNoGoroutine`, `TestAStatementByKeyWhoseContextEndedDoesNotRun`
- a statement that walks a range ends at its deadline — `TestARangeReadEndsAtItsDeadline`
- a point read is its statement's own snapshot — `TestALookupReadsEachStatementFromItsOwnSnapshot`

## KV

- a kv write that returned survives an abrupt exit — `TestAWriteThatReturnedSurvivesAnAbruptExit`, from many goroutines at once
- an expired key is absent to every operation — `TestAnExpiredKeyIsAbsentToEveryOperation`
- expiry past one pass's bound is taken in ten seconds — `TestAMaintainPastItsBoundIsFollowedSoon`, expired and cleared rows
- a default TTL is given once, at creation — `TestADefaultTTLIsGivenOnceAtCreation`
- an integer key is its decimal text — `TestAnIntegerKeyIsItsDecimalText`
- a kv version never repeats — `TestAVersionNeverRepeatsAfterDeleteExpiryOrReopen`
- a stale claim cannot finish or delete the next — `TestAStaleClaimCannotFinishOrDeleteTheNext`
- a kv Take whose value no longer decodes keeps it — `TestAFailedTakeKeepsItsValue`, a codec's panic and inside Tx included
- a kv value comes back as it went in — `TestAValueComesBackAsItWentIn`, floats by their bits, a named one's NaN that signals too
- a `kv.Raw` is what its row holds, for every type — `TestARawValueIsWhatItsRowHolds`, an empty string as empty bytes and not nothing
- an overflowing counter is refused, not rounded — `TestAnOverflowingCounterIsRefusedRatherThanRounded`
- `LoseAtMost` loses no more than its interval — `TestLoseAtMostLosesNoMoreThanItsInterval`, an exit that closes nothing
- counters of one name keep their numbers one way — `TestCountersOpenAgainOnlyAsTheyWereOpened`
- a `LoseAtMost` counter joins no transaction — `TestALoseAtMostCounterRefusesATransaction`
- counters in memory stay within their bound — `TestCountersInMemoryStayWithinTheirBound`, `TestFailedChangesStayWithinTheBound`
- cold counters arriving together pass no bound — `TestColdCountersArrivingTogetherStayWithinTheBound`
- a failed flush refuses new counters, loses none held — `TestAFailedFlushRefusesNewCountersRatherThanHoldThem`
- a kv Clear empties a branch and those under it — `TestClearEmptiesTheBranchAndThoseUnderIt`, over the bound and under it
- a cleared key is absent to every operation — `TestAClearedKeyIsAbsentToEveryOperation`
- a mark hides what lies under it at every depth — `TestAMarkHidesWhatLiesUnderItAtEveryDepth`, deeper than the lookups included
- a Clear never brings back counters waiting to flush — `TestAClearDoesNotResurrectCountersWaitingForTheFlush`
- a failed Clear keeps what counters wait to flush — `TestAFailedClearKeepsTheCountersWaitingForTheFlush`
- a Clear whose commit fails lets go as a crash would — `TestAClearWhoseCommitFailsLetsGoAsACrashWould`
- Clears beside changes and flushes keep branches apart — `TestClearsBesideChangesAndFlushesKeepTheirBranchesApart`
- a Clear inside Tx deletes what it clears or refuses — `TestAClearInATransactionOverTheBoundIsRefused`
- a call inside a kv Tx or View waits for no memory — `TestTxAndViewWaitForNoMemoryTheCallsWaitingForThemHold`
- kv holds the store's memory before it makes a value — `TestStoreMemoryBoundsWritesReadsAndScans`, `TestAWriteWaitingForMemoryHasEncodedNothing`
- a sliding read writes at most once per refresh — `TestASlidingReadWritesAtMostOncePerRefresh`
- a renewal never extends a newer incarnation of its key — `TestARenewalDoesNotExtendANewerIncarnation`, bound to version and expiry
- a key read in its last minute is renewed at once — `TestAReadNearItsExpiryRenewsAtOnce`
- kv's All holds no snapshot between its pages — `TestAllWalksEveryKeyAPageAtATime`
- a kv page ends before the value that passes its bytes — `TestAPageEndsBeforeTheValueThatPassesItsBytes`
- a config is its defaults, its environment, then what was kept, across a restart — `TestAConfigIsItsDefaultsThenItsEnvironmentThenWhatWasKept`, in both SDKs' suites too
- a config's layers go over each other in the order given, and no variable is read without one — `TestLayersGoOverEachOtherInTheOrderGiven`; `reads no variable without fromEnv, and a layer after it goes over it` in `sdk/js/test/config.test.ts`, `test_no_variable_is_read_without_from_env_and_a_layer_after_it_goes_over_it` in Python's
- a required setting is given by a layer, or the config does not open, naming its variable — `TestARequiredFieldIsGivenOrTheConfigDoesNotOpen`; `a required field is given by a layer, or the config does not open, naming its variable` in `sdk/js/test/config.test.ts`, `test_a_required_field_is_given_by_a_layer_or_the_config_does_not_open_naming_its_variable` in Python's
- a config change reaches every handle and watcher at once — `TestAChangeIsSeenByEveryHandleAtOnce`, `TestAConfigChangeReachesEveryWatcher` over the wire
- a config change that fails its check, or sets a secret, keeps nothing — `TestAChangeThatFailsItsCheckOrSetsASecretKeepsNothing`
- a fixed setting comes from the layers alone, and says where it came from — `TestAFixedFieldRefusesUpdateAndSaysWhereItCameFrom`; `a fixed field refuses update, ignores what was kept, and says where it came from` in `sdk/js/test/config.test.ts`, `test_a_fixed_field_refuses_update_ignores_what_was_kept_and_says_where_it_came_from` in Python's
- every variable that does not read, and every required setting missing, is said at once — `TestEveryBadVariableIsReportedAtOnce`; `every variable that does not read is said at once` in `sdk/js/test/config.test.ts`, `test_every_variable_that_does_not_read_is_said_at_once` in Python's
- NAME_FILE gives a setting its file's text, and NAME beside it is refused — `TestASecretReadsItsFile`; `a variable's file is read when NAME_FILE names it, and both set is refused` in `sdk/js/test/config.test.ts`, `test_a_variables_file_is_read_when_name_file_names_it_and_both_set_is_refused` in Python's
- a config's lookup is the only environment it reads — `TestAConfigsLookupIsTheOnlyEnvironmentRead`
- a kept value that no longer fits its field, or names a secret, is left out and named — `TestAKeptValueThatNoLongerFitsIsLeftOutAndNamed`, in both SDKs' suites too
- a variable is read by its field's type, or refused naming it — `TestAVariableIsReadByItsFieldsType`
- a config keeps only JSON within its bounds — `TestARawConfigKeepsOnlyJSONWithinItsBounds`
- variables and `.env` files read alike in every language — `TestVariablesAndDotenvFilesAreTheVectors`, over `kv/testdata/config.json`, which both SDKs' suites read
- a limiter lets its burst through, then its rate — `TestALimiterLetsABurstThroughThenItsRate`, `TestAllowNTakesAllOrNoneAndNeverPastTheBurst`, `TestALimiterOverTheWire`
- a limiter's times outlive a reopen, a quiet key is forgotten — `TestALimiterKeepsItsTimesAcrossAReopen`, `TestAQuietKeyIsForgottenOnceItsTimeHasCome`
- requests racing for a key pass no more than the burst — `TestRequestsRacingForAKeyPassNoMoreThanTheBurst`
- a quota counts a use in every window or in none, racing uses included — `TestAQuotaCountsInEveryWindowOrInNone`, `TestUsesRacingForAKeyPassNoMoreThanItsLimit`, `TestAQuotaOverTheWire`, `a use counts in every window or in none, and a refund gives it back` in `sdk/js/test/kv.test.ts`, `test_a_quota_counts_a_use_in_every_window_or_in_none` in Python's
- a quota's window starts at a key's first use after the last ended — `TestAWindowStartsAtTheFirstUseAfterTheLastEnded`
- a quota's `Get` counts nothing, a refund never goes below nothing, its windows outlive a reopen — `TestGetRefundAndDeleteChangeWhatTheySay`
- a quota that cannot count is refused at open — `TestAQuotaThatCannotCountIsRefused`
- a once key's function runs once and its answer is kept — `TestARunKeepsItsAnswerAndRunsAKeyOnce`, `TestAnErrorKeepsNothingAndTheNextRunRunsAgain`
- a run of a key waits for the one running it, every client's — `TestARunWaitsForTheRunOfItsKey`, `TestAWaitingRunEndsWithItsContextAndAnAnswerOutlivesIt`, `TestAOnceRunsAKeyOnceOverTheWire`, `a key runs once: a call meanwhile waits for its answer, and a throw keeps nothing` in `sdk/js/test/kv.test.ts`, `test_a_once_key_runs_once_and_a_call_meanwhile_waits_for_its_answer` in Python's

## Jobs

- a job in a batch commits with its rows or not at all — `TestAJobInABatchCommitsWithItsRows`, `TestAJobGoesOnlyInTheDatabaseItsQueueLivesIn`
- a job a Tx adds commits with its rows, and lets go of its turn either way — `TestAJobInATxCommitsWithItsRows`
- an Enqueue that returned survives an abrupt exit — `TestAnEnqueuedJobSurvivesAnAbruptExit`, from many goroutines at once
- a job runs at its time and not before — `TestAJobRunsAtItsTimeAndNotBefore`
- a write during a Work loop's read is not lost, nor keeps it awake — `TestAWriteDuringAnAlarmReadCannotBeLost`, `TestALaterWriteDuringAnAlarmReadLetsTheLoopSleep`
- a jobs Scan finds exactly the keys under its prefix — `TestScanFindsOnlyTheKeysUnderAPrefixNoRuneEnds`
- a key names one job, and enqueuing it only brings it forward — `TestAKeyNamesOneJobAndARepeatOnlyBringsItForward`
- an enqueue while its job runs asks for one run more — `TestAnEnqueueWhileItsJobRunsAsksForOneRunMore`
- `KeepDone` makes a key run once — `TestKeepDoneMakesAKeyRunOnce`
- `Update` changes only a job that still waits — `TestUpdateChangesOnlyAWaitingJob`
- `Cancel` says whether there was a job, and stops a running one's handler — `TestCancelSaysWhetherItCameInTime`, `TestCancelStopsTheHandlerOfARunningJob`
- a job claimed ahead for a busy worker waits, and a cancel keeps it from starting — `TestAJobHeldForABusyWorkerWaitsAndCancelKeepsItFromStarting`
- `Get` says where a job is and how many jobs run before it — `TestGetSaysWhereAJobIsAndHowManyRunBeforeIt`, `TestAJobWhoseLeaseEndedWaitsAgain`
- a watch follows its job to its end — `TestAWatchFollowsItsJobToItsEnd`
- a progress past 4 KiB is dropped in Go, refused by the SDKs — `TestAProgressPastItsBoundIsDropped`, `a progress JSON cannot write, or past 4 KiB, is refused` in `sdk/js/test/jobs.test.ts`
- `MaxRunning` holds a queue to its places across loops and claims — `TestMaxRunningHoldsAQueueToItsPlaces`
- a job keeps its last run, over the wire too, and a run given back records none — `TestAJobKeepsItsLastRun`, `TestAJobsLastRunOverTheWire`, `a job keeps its last run: when it began and how long it took` in `sdk/js/test/jobs.test.ts`, `test_a_job_keeps_its_last_run` in Python's
- a job whose lease ended runs again — `TestAJobWhoseLeaseEndedRunsAgain`
- a step runs once in a run, across its attempts and a worker that died, and a lost lease keeps none — `TestAStepRunsOnceAcrossTheAttemptsOfARun`, `TestAStepOfALostLeaseKeepsNothing`
- a run that ends takes its steps along, and a repeat's next run starts without them — `TestStepsGoWithTheirRun`, `TestAStepsNameAndAnswerAreBounded`
- a stale lease settles nothing — `TestAStaleLeaseSettlesNothing`
- a job that kills its process fails after its attempts — `TestAJobThatKillsItsProcessFailsAfterItsAttempts`
- a retry waits longer each time, then fails for good — `TestARetryWaitsLongerEachTimeThenFailsForGood`
- a snooze counts no attempt — `TestASnoozeCountsNoAttempt`
- a repeating job neither overlaps nor piles up — `TestARepeatingJobNeitherOverlapsNorPilesUp`
- a schedule keeps its zone across daylight saving — `TestAScheduleKeepsItsZoneAcrossDaylightSaving`
- `Work` settles by what the handler returns — `TestWorkSettlesByWhatTheHandlerReturns`, `UntilIdle` with one worker included
- jobs due together are claimed in batches — `TestJobsDueTogetherAreClaimedInBatches`
- a Work loop lets go of a lease another claim took — `TestWorkLetsGoOfALeaseAnotherClaimTook`, without writing again at once
- a handler stopped by `Close` gives its job back uncounted — `TestCloseGivesRunningJobsBackUncounted`
- a handler that returns as `Work` ends settles as it returned — `TestAHandlerThatReturnsAsWorkEndsSettlesAsItReturned`, done, failed, stopped
- a job value comes back as the JSON it went in — `TestAValueComesBackAsTheJSONItWentIn`
- a value that no longer reads fails its job, not its queue — `TestAValueThatNoLongerReadsFailsItsJob`, through Claim and Work
- a queue past `MaxWaiting` refuses the next job — `TestAQueuePastMaxWaitingRefusesTheNextJob`
- concurrent enqueues cannot pass `MaxWaiting` — `TestConcurrentEnqueuesCannotPassMaxWaiting`
- a failed job is kept, then removed — `TestAFailedJobIsKeptThenRemoved`
- a jobs Scan page holds at most its jobs and bytes — `TestAScanPageHoldsAtMostItsBytes`, spilled values counted
- a job's key left behind names nothing, then is dropped — `TestAKeyLeftBehindNamesNothingAndMaintenanceDropsIt`
- a job that moves takes its key along — `TestAMovedJobTakesItsKeyAlong`
- jobs hold values in the store's memory — `TestStoreMemoryBoundsEnqueuesReadsAndHandlers`
- an Enqueue waiting for memory has written nothing — `TestAnEnqueueWaitingForMemoryHasWrittenNothing`
- a jobs Tx takes only the memory that is free — `TestATransactionTakesOnlyTheMemoryThatIsFree`

## Blobs

- a Put that returned survives an abrupt exit — `TestAPutThatReturnedSurvivesAnAbruptExit`, inline and in files
- an object appears whole at its commit or not at all — `TestAnObjectAppearsWholeAtItsCommitOrNotAtAll`, readers racing its replacements
- an exit at any step of an upload leaves nothing behind — `TestAnExitAtEveryStepOfAnUploadLeavesNothingBehind`, the next Open cleaning up
- Open walks only what a crash can have left — `TestOpenRemovesWhatAbandonedUploadsLeft`, past the settled mark alone
- a commit whose outcome is unknown leaves no file — `TestAnUnknownCommitLeavesNoFileBehind`
- an upload that does not commit leaves nothing — `TestAnAbortedOrAbandonedUploadLeavesNothing`, `TestMaintenanceAbortsAnUploadItsContextLeft`
- a reader keeps what it opened, on Windows too — `TestAReaderKeepsWhatItOpened`, through a delete, a replace, an expiry, a Clear
- an Open racing a replace opens the new object — `TestAnOpenRacingAReplaceOpensTheNewObject`
- a whole read of a changed byte fails before its end — `TestAWholeReadOfAChangedByteFailsBeforeItsEnd`; a range is not checked
- the scrub finds what changed and names its keys — `TestTheScrubNamesTheKeysOfWhatChanged`, `TestTheScrubKeepsItsPlaceAcrossReopens`
- memory does not follow an object's size — `TestMemoryDoesNotGrowWithAnObjectsSize`, `TestStoreMemoryBoundsUploadsReadsAndScans`
- a Put grows its buffer only when memory is free now — `TestStreamingUsesOnlyTheBufferItsBudgetCanHold`, `TestAFailedStreamReleasesItsLargerBuffer`
- a Windows scanner's hold is retried and cleaned up — `TestARenameRetriesAfterAWindowsScannerLetsGo`, `TestAHeldRenameExhaustsRetriesAndRecovers`
- a stream that disagrees with its Size is refused — `TestAStreamThatDisagreesWithItsSizeIsRefused`
- an upload past MaxSize or KeepFree leaves nothing — `TestAnUploadPastItsBoundsStopsAndLeavesNothing`
- of two conditional replaces, one conflicts — `TestOneOfTwoConditionalReplacesConflicts`, `TestIfNoneMatchCreatesOnce`
- a copy shares the bytes and outlives its source — `TestACopySharesTheBytesAndOutlivesItsSource`
- a blobs Clear empties a folder and those under it — `TestClearEmptiesAFolderAndThoseUnderIt`, over the bound and under it
- a key is its own bytes on every file system — `TestAKeyIsItsOwnBytesOnEveryFileSystem`, `TestAPathThatIsNotOneIsRefused`
- a file another program holds is removed later — `TestAFileHeldElsewhereIsRemovedLater`, on Windows
- a snapshot links the files, and removal waits for it — `TestASnapshotLinksFilesAndCopiesTheDatabase`, `TestCollectionWaitsForASnapshot`
- the blobs engine links no net/http — `TestBlobsImportsNoHTTP`

## Backups

- a backup restores every engine — `TestABackupRestoresEveryEngine`
- a changed backup is refused and leaves nothing — `TestAChangedByteIsRefusedAndLeavesNothing`
- a backup restores every object bit for bit — `TestABackupRestoresEveryObject`; past 4 GiB when `TINYSTORE_LARGE_BACKUP` is set
- a backup holds a file of the host's only when it is named, and restores it checked — `TestABackupKeepsANamedHostFileAndRestoresIt`, `TestAHostFileOutsideTheStoreIsRefused`; over the wire `TestABackupOverTheWireKeepsANamedHostFile`, `TestBackupKeepsAFileItIsGiven` in `cmd/tinystore`, `a backup keeps a file of the application's only when files names it` in `sdk/js/test/backup.test.ts`, `test_a_backup_keeps_a_file_of_the_applications_only_when_files_names_it` in Python's

## The server and the wire

- a job in an SQL batch over the wire commits with its rows, on the program's own store when it passed one — `TestAJobInAnSQLBatchCommitsWithItsRows`, `TestAQueueInTheProgramsDatabaseIsTheProgramsOwn`; `a job a batch enqueues commits with the rows or not at all` in `sdk/js/test/sql.test.ts`, `test_a_job_a_batch_enqueues_commits_with_the_rows_or_not_at_all` in Python's
- a backup over the wire holds every engine on disk, and makes none that is not — `TestABackupOverTheWireHoldsEveryEngineOnDisk`; `TestBackupWritesAZipThatRestoreTakesBack` in `cmd/tinystore`; `a backup is one zip of every engine, which restore takes back into an empty directory` in `sdk/js/test/backup.test.ts`, `test_a_backup_is_one_zip_of_every_engine_which_restore_takes_back` in Python's
- a download holds the store's memory until its last DATA — `TestADownloadHoldsTheStoresMemoryUntilItsLastData`
- a watch ends with its client's side — `TestAWatchEndsWithItsClientsSide`
- a remote worker's job is watched, and its handler told of a cancel — `TestAJobWatchFollowsARemoteWorkersJob`, `a watch follows a job up its queue, through its progress, to a cancel its handler sees` in `sdk/js/test/jobs.test.ts`, `test_a_watch_follows_a_job_through_its_progress_to_a_cancel_its_handler_sees` in Python's
- a remote worker's steps are kept across a run's attempts, claimed or on a work stream — `TestAJobsStepsOverTheWire`, `a step runs once in a run: the attempt after a failure gets its kept answer` in `sdk/js/test/jobs.test.ts`, `test_a_step_runs_once_in_a_run_the_attempt_after_a_failure_gets_its_kept_answer` in Python's
- a frame past its agreed size is refused unread — `TestAFrameLargerThanAgreedIsRefusedUnread`, `FuzzFrames` in `server/wire`
- a body the profile does not allow is refused — `FuzzMessages`, a refused vector for each rule
- a request is understood whole or refused, naming the field and the server's version — `TestARequestWithAFieldTheServerDoesNotKnowIsRefused`, `TestAMessageReadsWhatItKnowsAndNamesWhatItDoesNot`
- a client newer than its server speaks the server's protocol — `TestAClientOfANewerProtocolIsWelcomedInTheServers`
- the vectors are the bytes — `TestVectors`, `TestFrameVectors`, `TestTheExamplesAreWhatTheMessagesWrite`
- every message is a vector, every field by its name — `TestMessageVectors`: `messages.json` is what the Go types write, each schema field in one
- the largest kv or jobs value travels in one body — `TestTheLargestValueTravelsInOneBody`
- a work stream asked to end when idle ends — `TestAWorkStreamUntilIdleEndsOnceNoJobIsDue`
- an extend on a work stream is refused, not an ack — `TestAnExtendOnAWorkStreamIsRefused`
- a client past its credit is cut off, the reader never waits — `TestAClientPastItsCreditIsCutOff`, `TestFramesThatBreakTheProtocolEndTheConnection`
- every stream ends with one final frame — `TestEveryStreamEndsOnce`, answered, failed, panicked, cancelled, silent, down and up
- answers queued during a write leave in the next — `TestQueuedAnswersShareAWrite` in `server/internal/flow`
- a connection grant wakes every sender whose body fits — `TestAGrantWakesEverySenderWhoseBodyFits` in `server/internal/flow`
- a Go test client's upload stops with its stream or connection — `TestAFinalResponseStopsAnUploadWaitingForCredit`, `TestALostConnectionStopsAnUploadWaitingForCredit` in `server/internal/client`
- a point read cancelled while it waits lets its connection go on — `TestACancelledPointReadLetsTheConnectionGoOn`
- a stream's number is free when its final frame arrives — `TestAStreamNumberIsFreeWhenItsFinalFrameArrives`
- a closing server lets the streams running finish — `TestClosingTheServerLetsTheStreamsRunningFinish`, `TestARequestThatCrossesTheGoAwayIsAnsweredUnavailable`
- a remote connection needs its token — `TestARemoteConnectionNeedsItsToken`
- a pipe's name has one owner — `TestAPipesNameHasOneOwner`, on Windows
- a kv batch is one transaction — `TestAKVBatchRollsBackWhenOneOfItsCallsFails`
- a read that names what it read fails once its key changed, in a batch too — `TestAReadThatNamesWhatItReadFailsOnceTheKeyChanged`
- Go and a wire client read each other's kv buckets — `TestAWireClientAndAGoProgramReadEachOthersBuckets`
- a remote worker's outcomes settle its jobs — `TestARemoteWorkerSettlesByItsOutcomes`, a retry counted
- a lost connection aborts uploads, fails attempts in hand — `TestALostConnectionAbortsUploadsAndFailsAttemptsInHand`, `TestALostWorkerFailsTheAttemptsInItsHands`
- a jobs enqueue of many is one transaction — `TestAJobsEnqueueIsOneTransaction`
- a blobs put that does not commit leaves nothing — `TestAnUploadThatDoesNotCommitLeavesNothing`
- a whole blobs get of a changed object ends corrupt — `TestAWholeReadOfAChangedObjectEndsCorrupt`
- the server module requires only the root — `TestTheServerRequiresOnlyTheRoot`
- `server/wire` imports only the standard library — `TestWireImportsOnlyTheStandardLibrary`
- a data client cannot change a schema — `TestADataClientCannotChangeTheSchema`, `TestEachLineOfTheCheckRefusesOnItsOwn`, `FuzzDataSQL`
- the check ends a statement where SQLite does — `TestSQLiteEndsAStatementWhereTheCheckDoes`, `TestTheCheckReadsSQLitesTokens`
- a database opens once, and later opens check it — `TestSQLOpenAppliesOnceAndChecksAfter`, `TestADatabaseTheProgramOpenedIsChecked`
- an sql batch is one transaction, a read one snapshot — `TestAnSQLBatchIsOneTransaction`
- a data connection's statement ends at its deadline — `TestADataStatementEndsAtItsDeadline`
- an answer past the agreed body fails only its stream — `TestAnAnswerPastTheBodyIsALimit`
- a record over the wire comes back as it went in — `TestRecordsOverTheWire`, times to the nanosecond, bytes that are not UTF-8
- an append over the wire names the record it refused — `TestARefusedRecordNamesItsPlace`, all or none
- a follower over the wire comes back where it stopped — `TestFollowOverTheWire`
- another program's lines over the wire become records — `TestLinesOverTheWire`, `TestLinesHandOverWhatTheyHoldWhenTheUploadEnds`
- a records drop is an admin's repair — `TestADropIsAnAdminsRepair`, `TestADamagedRowIsNamedAsADropNamesIt`
- a sample over the wire comes back bit for bit — `TestMetricsOverTheWire`, -0 and a NaN's payload included
- a metrics ingest over the wire is all or none — `TestAMetricsIngestIsAllOrNone`, the refused series named by its labels
- an aggregate over the wire counts resets in its bucket — `TestAggregateOverTheWire`
- a series longer than a body comes in pieces — `TestALongSeriesComesInPieces`
- `SERVE` is written whole, under the lock, for its owner — `TestServeIsWrittenWholeUnderTheLock`, `TestADirectoryIsItsOwnersAlone`
- a `SERVE` left behind goes before its server listens — `TestAServeLeftBehindGoesBeforeTheServerListens`
- a client reading `SERVE` delays a change, fails none — `TestAChangeHeldUpByAReaderIsTriedAgain`, `TestAServeHeldPastEveryTryIsAnError`
- a server goes idle only after its last connection — `TestAServerGoesIdleAfterItsLastConnection`
- a program shares its own store in one call, until stop — `TestShareServesTheProgramsStoreUntilItStops`
- an engine a program opened and did not pass says to pass it — `TestAnEngineTheProgramOpenedAndDidNotPassSaysToPassIt`
- a long store's socket moves to the user's own directory — `TestALongSocketPathMovesToTheUsersOwnDirectory`, off Windows
- a server proves it read `SERVE` to a local challenge only — `TestAServerProvesItselfOnlyToALocalChallenge`, `TestProofVectors`
- an endpoint taken after its server left cannot prove itself — `TestAnEndpointTakenAfterItsServerLeftCannotProveItself`
- a test's clock moves only forward, an admin's alone, and the store runs on it — `TestATestsClockMovesOnlyForwardAndTheStoreRunsOnIt`, `TestAPrivateChildRunsOnTheClockItIsGiven` in `cmd/tinystore`; `a private store runs on the clock it is given, which moves only forward` in `sdk/js/test/clock.test.ts`, `test_a_private_store_runs_on_the_clock_it_is_given_which_moves_only_forward` in Python's
- a server stops at an admin's request, and only when its program said how — `TestAServerStopsAtAnAdminsRequestOnly`, `TestStopEndsTheServerOfADirectory` in `cmd/tinystore`

## The tinystore command

- the tool finds each database by its name — `TestTheToolFindsEachDatabaseByItsName`, `TestTheToolWritesTheNextMigrationThroughTheCheck` in `cmd/tinystore`
- a remote server is bounded unless told otherwise — `TestARemoteServerIsBoundedUnlessToldOtherwise` in `cmd/tinystore`: 1 GiB with `--listen`
- a sidecar started in the background says why it ended — `TestServeLogsToTheFileItIsGivenWhyItEnded` in `cmd/tinystore`
- of two sidecars started at once, one exits held — `TestAStaleServeStartsOneSidecar`, `TestASecondServeOfADirectoryExitsHeld`
- a sidecar leaves once idle, with its `SERVE` and lock — `TestTheSidecarIsFoundThroughServeAndLeavesWhenIdle`
- a private child leaves when told, though its parent stays — `TestAPrivateChildLeavesWhenToldThoughItsParentStays`, `TestAPrivateChildServesItsParent`
- what serve cannot serve opens nothing — `TestServeRefusesWhatItCannotServe`, a file it cannot read included
- the tool requires only the store and the server — `TestTheToolRequiresOnlyTheStoreAndTheServer`
- status reads a directory beside its server, for a person and with `--json`, never printing SERVE's secret — `TestStatusReadsADirectoryAndKeepsTheSecret` in `cmd/tinystore`
- the command alone lists its commands, a directory before or after the flags — `TestHelpListsTheCommandsAndIsNoError`, `TestADirectoryComesBeforeOrAfterTheFlags` in `cmd/tinystore`
- `serve <dir>` serves the directory until Ctrl+C, never idle — `TestServeOfADirectoryServesItUntilCtrlC`
- SERVE calls a sidecar one, and neither a person's server nor a program's — `TestTheSidecarIsFoundThroughServeAndLeavesWhenIdle`, `TestServeOfADirectoryServesItUntilCtrlC` in `cmd/tinystore`, `TestServeIsWrittenWholeUnderTheLock`
- a read through the tool makes no store of a directory — `TestLogsOfADirectoryWithoutAStoreMakeNone`
- `logs -f` prints a record sent a little late, each once and two alike twice — `TestLogsPrintTheLastRecordsAndFollowTheNext`
- an agent over MCP writes nothing, nor makes an engine's file — `TestAnAgentReadsTheStoreOverMCP`, `TestAnAgentMakesNoEnginesFile`

## The SDKs

- a timer's measure answers, rethrows, and records either way — `a timer writes its count, sum and longest at each flush; measure answers and rethrows` in `sdk/js/test/records.test.ts`, `test_a_timer_writes_its_count_sum_and_longest_at_each_flush` in Python's
- a Bun batch or view gives back what its function returns, answered — `a view and a batch give back what their function returns, its promises answered` in `sdk/js/test/kv.test.ts`, `a batch and a view give back what their function returns, its promises answered` in `sdk/js/test/sql.test.ts`
- a Bun logger's line never waits for the server — `a full logger drops and counts rather than wait` in `sdk/js/test/records.test.ts`
- a Bun logger writes what filled its buffer during a write as that write ends — `what filled the buffer while a write ran goes as the write ends, not at the next second` in `sdk/js/test/logger.test.ts`
- a Bun call under an aborted signal is refused — `every call under an aborted signal is refused` in `sdk/js/test/records.test.ts`
- a Bun duration or rate misspelled does not compile — `a duration misspelled does not compile, and text from elsewhere is checked at its call` in `sdk/js/test/kv.test.ts`, `refuses a rate it cannot read, its type before its call` in `sdk/js/test/config.test.ts`
- a Bun store's close frees its directory — `close returns once the child has exited` in `sdk/js/test/kv.test.ts`
- the JS SDK runs under Node: its sidecar, a private child, TCP and TLS checked — `a remote server is reached over TLS, its certificate checked, under Node` and the rest of `sdk/js/test/under-node.ts`, run by `node --test`
- a Python line takes the fields of the context it was logged in — `test_lines_take_the_fields_of_the_context_they_were_logged_in` in Python's suite
- a byte the Bun encoder writes as its buffer grows is kept — `a byte written as the buffer grows is kept` in `sdk/js/test/wire.test.ts`
- an SDK's tx reads, decides and writes, running again when a key it read changed, five times at most — `transactions` in `sdk/js/test/kv.test.ts`, `test_a_tx_reads_decides_and_writes_and_runs_again_when_a_key_it_read_changed` and `test_a_tx_reads_its_own_writes_and_gives_up_on_a_key_that_keeps_changing` in Python's
- an SDK replaces a sidecar of an older release than its own, its clients moving to the new one; another server it only tells of — `a sidecar of an older release is stopped, and every client moves to the one started in its place` and `a server of an older release than its SDK is told apart, and only one` in `sdk/js/test/connection.test.ts`, `test_a_sidecar_of_an_older_release_is_stopped_and_every_client_moves_to_the_one_started_in_its_place` in Python's
- the SDK packages install the `tinystore` command, the binary's output and exit code its own — `the tinystore command runs the binary, with its output and its exit code` in `sdk/js/test/command.test.ts`, under Bun and Node, `test_the_tinystore_command_runs_the_binary_its_output_and_exit_code_the_binarys` in Python's

## The docs site

- a link in the docs that leads nowhere fails the site's build — `that leads nowhere is a problem` in `web/src/lib/content/links.test.ts`, `resolves and checks the links inside a table` in `web/src/lib/content/markdown.test.ts`, and the prerender `task web` runs
- the API pages are what the Go, Bun and Python source says — `are what task reference writes from the source` in `web/src/lib/content/reference.test.ts`
- every markdown table is aligned — `every table of the repository's markdown is aligned: task tables aligns them` in `web/src/lib/content/tables.test.ts`
- a heading keeps the anchor GitHub gives it — `gives headings the ids GitHub gives them, a repeated one numbered` in `web/src/lib/content/outline.test.ts`
- fences in three languages are one block, untitled ones of a language two — `a run in three languages is one block`, `two untitled fences of one language stay two blocks` in `web/src/lib/content/code.test.ts`
- the landing page's numbers are the README's SVG cards, read back — `read back from every SVG the README shows, in its order` in `web/src/lib/content/landing.test.ts`
- the README's headline, pitch, sample and engines are the landing page's, and the SDK packages' in their language, and no link in them leads nowhere — `is what task readme writes into it from web/landing.md`, `has no link that leads nowhere`, `links only to what exists, on GitHub` in `web/src/lib/content/readme.test.ts`
- the landing page's words are `web/landing.md`'s, a part missing or a link nowhere failing the build — `takes every word from web/landing.md, its links resolved as a page's`, `fails the build when a part is missing or a link leads nowhere` in `web/src/lib/content/landing.test.ts`
- the site's views and events live in its own TinyStore, through a restart — `keeps a view and an event as records and counts both, through a restart` in `web/server/analytics.test.ts`
- a page, its data and its markdown are views; an asset, a HEAD and a 404 are not — `is served and counted, as are its data and its markdown; an asset is not counted` in `web/server/http.test.ts`

## Releases

- a release's version is one every registry spells, after every release before it — `TestOnlyAPreReleaseEveryRegistrySpellsIsPublished`, `TestVersionsOrderAsSemanticVersioningSays`
- what a release publishes says its version, stamped as it is built — `TestTheSDKPackageCarriesTheReleasesVersion`, `TestAPyprojectIsStampedWithTheReleasesVersion`
- a platform wheel is one PyPI takes: every file in RECORD, each file's sizes before its bytes — `TestAPlatformWheelRecordsEveryFile`
- `feat` and `fix` subjects are a release's notes — `TestNotesKeepFeaturesAndFixesUnderTheirSections`, `TestNotesBeginAfterThePreviousReleaseOfTheirKind`
