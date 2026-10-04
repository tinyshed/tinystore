"""The names a LimitError's limit holds, as Go's constants and Bun's limits spell them.

Map a limit without matching its text::

    except tinystore.LimitError as err:
        if err.limit == tinystore.limits.DECODED_SAMPLES:
            ...
"""

from typing import Final

STORE_MEMORY: Final = "store memory"
STORE_MEMORY_NOW: Final = "store memory, now"
MATCHED_SERIES: Final = "matched series"
DECODED_BLOCKS: Final = "decoded blocks"
FETCHED_BYTES: Final = "fetched bytes"
DECODED_SAMPLES: Final = "decoded samples"
OUTPUT_SAMPLES: Final = "output samples"
OUTPUT_BUCKETS: Final = "output buckets"
RECORD_BYTES: Final = "bytes of a record, a block's"
APPEND_BYTES: Final = "bytes of records in one Append, a segment's; split it"
JOB_VALUE_BYTES: Final = "bytes of a job's value"
STEP_BYTES: Final = "bytes of a step's answer"
OBJECT_BYTES: Final = "bytes of an object, the bucket's MaxSize"
