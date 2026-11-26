"""Summarize frozen per-image COCO samples without changing their labels."""

from collections import Counter
from collections.abc import Iterable
from dataclasses import dataclass

if __package__:
    from .image_inspection import ImageInspectionError, inspect_image
else:
    from image_inspection import ImageInspectionError, inspect_image


@dataclass(frozen=True, slots=True)
class ProfileReadFailure:
    code: str


@dataclass(frozen=True, slots=True)
class ProfileSample:
    asset_id: str
    image: bytes | ProfileReadFailure
    annotation: dict | ProfileReadFailure | None


@dataclass(frozen=True, slots=True)
class ProfileFileError:
    asset_id: str
    source: str
    code: str


@dataclass(frozen=True, slots=True)
class DatasetProfile:
    asset_count: int
    annotation_count: int
    resolution_histogram: dict[tuple[int, int], int]
    class_counts: dict[str, int]
    empty_annotation_count: int
    unannotated_count: int
    file_errors: tuple[ProfileFileError, ...]


def profile_dataset(samples: Iterable[ProfileSample], *, max_pixels: int) -> DatasetProfile:
    if max_pixels <= 0:
        raise ValueError("max_pixels must be positive")
    asset_count = annotation_count = empty_count = unannotated_count = 0
    resolutions = Counter()
    classes = Counter()
    errors = []
    for sample in samples:
        asset_count += 1
        if isinstance(sample.image, ProfileReadFailure):
            errors.append(ProfileFileError(sample.asset_id, "image", sample.image.code))
        else:
            try:
                image = inspect_image(sample.image, max_pixels=max_pixels)
                resolutions[image.width, image.height] += 1
            except ImageInspectionError as error:
                errors.append(ProfileFileError(sample.asset_id, "image", error.code))

        if sample.annotation is None:
            unannotated_count += 1
            continue
        if isinstance(sample.annotation, ProfileReadFailure):
            errors.append(ProfileFileError(sample.asset_id, "annotation", sample.annotation.code))
            continue
        try:
            names = annotation_class_names(sample.annotation)
        except KeyError:
            errors.append(ProfileFileError(sample.asset_id, "annotation", "unknown_category"))
            continue
        except (TypeError, ValueError):
            errors.append(ProfileFileError(sample.asset_id, "annotation", "invalid_annotation"))
            continue
        annotation_count += len(names)
        empty_count += not names
        classes.update(names)

    return DatasetProfile(
        asset_count, annotation_count, dict(sorted(resolutions.items())), dict(sorted(classes.items())),
        empty_count, unannotated_count, tuple(errors),
    )


def annotation_class_names(document: dict) -> list[str]:
    try:
        categories = document["categories"]
        annotations = document["annotations"]
        if not isinstance(categories, list) or not isinstance(annotations, list):
            raise ValueError("expected COCO categories and annotations")
        names = {item["id"]: item["name"] for item in categories}
        if len(names) != len(categories) or any(not isinstance(name, str) or not name for name in names.values()):
            raise ValueError("invalid COCO category definitions")
        category_ids = [item["category_id"] for item in annotations]
    except KeyError as error:
        raise ValueError("incomplete COCO annotation") from error
    return [names[category_id] for category_id in category_ids]
