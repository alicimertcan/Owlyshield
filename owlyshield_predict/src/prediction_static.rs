use std::collections::HashMap;
use std::path::Path;
use byteorder::{ByteOrder, LittleEndian};
use moonfire_tflite::{Interpreter, Model};
use win_pe_inspection::LibImport;


static MALAPI: &'static [u8] = include_bytes!("../models/malapi.json");
/// The .tflite (converted from Tensorflow/Keras) model is included as a static variable.
static MODEL: &'static [u8] = include_bytes!("../models/model_static.tflite");
/// Features means vector, used by Standard Scaling.
static MEANS: &'static [u8] = include_bytes!("../models/mean_static.json");
/// Features standard deviations vector used by Standard Scaling.
static STDVS: &'static [u8] = include_bytes!("../models/std_static.json");

pub struct TfLiteStatic {
    model: Model,
    /// Needed by Standard Scaling and set to [MEANS]
    means: Vec<f32>,
    /// Needed by Standard Scaling and set to [STDVS]
    stdvs: Vec<f32>,
    malapi: HashMap<String, Vec<String>>,
}

impl TfLiteStatic {
    pub fn new() -> TfLiteStatic {
        let means = serde_json::from_slice(MEANS);
        let stdvs = serde_json::from_slice(STDVS);
        let malapi = serde_json::from_slice(MALAPI);

        TfLiteStatic {
            model: Model::from_static(MODEL).unwrap(),
            means: means.unwrap(),
            stdvs: stdvs.unwrap(),
            malapi: malapi.unwrap(),
        }
    }

    pub fn make_prediction(&self, path: &Path) -> Option<f32> {
        if let Ok(static_features) = win_pe_inspection::inspect_pe(path) {
            let mut input_vec = vec![
                static_features.data_len as f32,
                static_features.section_table_len as f32,
                static_features.has_dbg_symbols as u32 as f32
            ];
            let mut import_cats_cnt = self.count_imports_by_categories(&static_features.imports);
            input_vec.append(&mut import_cats_cnt);
            let input_vec_scaled = self.stdscale_transform(&input_vec);

            let builder = Interpreter::builder();
            let mut interpreter = builder
                .build(&self.model, 1, input_vec_scaled.len())
                .unwrap();

            let mut inputs = interpreter.inputs();
            let mut dst = inputs[0].bytes_mut();
            LittleEndian::write_f32_into(input_vec_scaled.as_slice(), &mut dst);
            interpreter.invoke().unwrap();
            let outputs = interpreter.outputs();

            let y_pred = outputs[0].f32s()[0];
            Some(y_pred)
        } else {
            None
        }
    }

    fn count_imports_by_categories(&self, imports: &Vec<LibImport>) -> Vec<f32> {
        let keys_count = self.malapi.keys().len();
        let mut res = Vec::with_capacity(keys_count);
        res.resize(keys_count, 0.0);
        let mut i = 0;
        let mut keys: Vec<&String> = self.malapi.keys().collect();
        keys.sort();
        for key in keys {
            for import in imports {
                let fnames = &self.malapi[key];
                if fnames.contains(&import.import) {
                    res[i] += 1.0;
                }
            }
            i += 1;
        }
        res
    }

    fn stdscale_transform(&self, input_vec: &Vec<f32>) -> Vec<f32> {
        let epsilon = 0.0001f32;
        input_vec.iter()
            .enumerate()
            .map(|(i, x)| {
                let stdvi = self.stdvs[i];
                let denominator = if stdvi < epsilon { epsilon } else { stdvi };
                (x - self.means[i]) / denominator
            }).collect::<Vec<_>>()
    }

    // fn standardize(&self, predmtrx: &VecvecCapped<f32>) -> VecvecCapped<f32> {
    //     let mut res = predmtrx.clone();
    //     let epsilon = 0.0001f32;
    //     for i in 0..predmtrx.rows_len() {
    //         //predmtrx.capacity_rows {
    //         for j in 0..predmtrx.capacity_cols {
    //             let stdvs_j = self.stdvs[j];
    //             let denominator = if stdvs_j < epsilon { epsilon } else { stdvs_j };
    //             res[i][j] = (predmtrx[i][j] - self.means[j]) / denominator
    //         }
    //     }
    //     res
    // }

}


