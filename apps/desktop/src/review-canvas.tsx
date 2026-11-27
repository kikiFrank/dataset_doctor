import { useRef, useState } from "react";
import type { PointerEvent } from "react";
import type { CocoImage } from "./api/generated/wire-primitives";
import { boxFromDrag, translateBoxFromDrag } from "./review-document";
import type { BoxCoordinates, EditableBox, ImagePoint } from "./review-document";

export function ReviewCanvas({ image, locator, boxes, selected, disabled, onSelect, onDraw, onMove, onImageError }: {
  image: CocoImage;
  locator: string;
  boxes: EditableBox[];
  selected: number | null;
  disabled: boolean;
  onSelect: (index: number) => void;
  onDraw: (coordinates: BoxCoordinates) => void;
  onMove?: (index: number, coordinates: BoxCoordinates) => void;
  onImageError: () => void;
}) {
  const [showBoxes, setShowBoxes] = useState(true);
  const [zoom, setZoom] = useState("100");
  const [preview, setPreview] = useState<BoxCoordinates | null>(null);
  const drag = useRef<{ pointer: number; start: ImagePoint; boxIndex: number | null } | null>(null);

  function imagePoint(event: PointerEvent<SVGSVGElement>): ImagePoint | null {
    const svg = event.currentTarget;
    const matrix = svg.getScreenCTM();
    if (!matrix) return null;
    const point = new DOMPoint(event.clientX, event.clientY).matrixTransform(matrix.inverse());
    return [point.x, point.y];
  }

  function start(event: PointerEvent<SVGSVGElement>) {
    if (disabled || !showBoxes || event.button !== 0 || drag.current) return;
    const point = imagePoint(event);
    if (!point || point[0] < 0 || point[1] < 0 || point[0] > image.width || point[1] > image.height) return;
    const index = event.target instanceof SVGRectElement ? event.target.dataset.boxIndex : undefined;
    const boxIndex = index === undefined ? null : Number(index);
    if (boxIndex !== null) {
      onSelect(boxIndex);
      if (!onMove) return;
    }
    event.currentTarget.setPointerCapture(event.pointerId);
    drag.current = { pointer: event.pointerId, start: point, boxIndex };
  }

  function coordinates(active: NonNullable<typeof drag.current>, end: ImagePoint) {
    if (active.boxIndex === null) return boxFromDrag(active.start, end, image);
    const box = boxes[active.boxIndex];
    return box ? translateBoxFromDrag(box.bbox, active.start, end, image) : null;
  }

  function move(event: PointerEvent<SVGSVGElement>) {
    if (drag.current?.pointer !== event.pointerId) return;
    const end = imagePoint(event);
    if (end) setPreview(coordinates(drag.current, end));
  }

  function finish(event: PointerEvent<SVGSVGElement>) {
    const active = drag.current;
    if (active?.pointer !== event.pointerId) return;
    const end = imagePoint(event);
    const box = end ? coordinates(active, end) : null;
    drag.current = null;
    setPreview(null);
    if (!box || disabled || !showBoxes) return;
    if (active.boxIndex === null) onDraw(box);
    else onMove?.(active.boxIndex, box);
  }

  return (
    <div className="review-canvas">
      <div className="review-actions">
        <label><input type="checkbox" checked={showBoxes} onChange={(event) => setShowBoxes(event.target.checked)} /> 显示框</label>
        <label>缩放 <select value={zoom} onChange={(event) => setZoom(event.target.value)}>
          <option value="100">适合宽度</option><option value="150">150%</option><option value="200">200%</option>
        </select></label>
      </div>
      <div className="review-viewport" aria-label="图像视口">
        <svg
          className={`review-image zoom-${zoom}`}
          viewBox={`0 0 ${image.width} ${image.height}`}
          aria-label={onMove ? "原图画布，拖动可补框或移动框" : "原图画布，拖动可补框"}
          onPointerDown={start} onPointerMove={move} onPointerUp={finish}
          onLostPointerCapture={() => { drag.current = null; setPreview(null); }}
        >
          <image href={locator} width={image.width} height={image.height} onError={onImageError} />
          {showBoxes && boxes.map(({ bbox }, index) => (
            bbox.every(Number.isFinite) && bbox[2] > 0 && bbox[3] > 0 && (
              <rect key={index} x={bbox[0]} y={bbox[1]} width={bbox[2]} height={bbox[3]}
                className={selected === index ? "selected-box" : undefined}
                vectorEffect="non-scaling-stroke" data-box-index={index} />
            )
          ))}
          {preview && <rect x={preview[0]} y={preview[1]} width={preview[2]} height={preview[3]}
            className="selected-box preview-box" vectorEffect="non-scaling-stroke" />}
        </svg>
      </div>
      <small>拖动空白处补框。{onMove && "拖动已有框可移动。"}放大后可滚动查看图像。</small>
    </div>
  );
}
